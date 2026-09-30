//! Admission is locked together with inference-guard acquisition. Waiting leases
//! expire; a committed external stop never expires, because a delayed OS stop
//! must not kill inference admitted after that expiry.

use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const LEASE_TTL: Duration = Duration::from_secs(30);
const MIN_COMMIT_ROOM: Duration = Duration::from_secs(5);
const MAX_WAIT_SECS: u64 = 7200;

#[derive(Default)]
pub struct Admission {
    epoch: u64,
    legacy: bool,
    lease: Option<Lease>,
}

struct Lease {
    owner: String,
    expires: Instant,
    deadline: Instant,
    committed: bool,
}

impl Admission {
    fn expire(&mut self, now: Instant) {
        if self
            .lease
            .as_ref()
            .is_some_and(|l| !l.committed && now >= l.expires)
        {
            self.lease = None;
        }
    }

    pub fn blocked(&mut self, now: Instant) -> bool {
        self.expire(now);
        self.legacy || self.lease.is_some()
    }

    pub fn snapshot(&mut self, now: Instant) -> Value {
        self.expire(now);
        let (phase, remaining_ms, deadline_ms) = match &self.lease {
            Some(l) if l.committed => ("committed", None, None),
            Some(l) => (
                "waiting",
                Some(l.expires.saturating_duration_since(now).as_millis()),
                Some(l.deadline.saturating_duration_since(now).as_millis()),
            ),
            None if self.legacy => ("legacy_idle", None, None),
            None => ("open", None, None),
        };
        // The owner token is deliberately absent from public status.
        json!({"protocol":1,"epoch":self.epoch,"phase":phase,"remaining_ms":remaining_ms,"deadline_remaining_ms":deadline_ms})
    }

    pub fn legacy(&mut self, drain: bool, active: u64, now: Instant) -> Result<(), &'static str> {
        self.expire(now);
        if self.lease.is_some() {
            return Err("a drain lease owns admission; legacy control cannot change it");
        }
        if drain && active != 0 {
            return Err("inference is active; service was preserved");
        }
        self.legacy = drain;
        Ok(())
    }

    /// Caller validates the process incarnation and owns the admission mutex.
    pub fn control(
        &mut self,
        action: &str,
        request: &Value,
        active: u64,
        now: Instant,
    ) -> Result<Value, &'static str> {
        self.expire(now);
        let owner = request["owner"]
            .as_str()
            .filter(|s| {
                (16..=128).contains(&s.len())
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
            .ok_or("invalid drain owner token")?;
        let epoch = request["epoch"]
            .as_u64()
            .ok_or("a drain epoch is required")?;
        if action == "acquire" {
            if self.legacy {
                return Err("legacy idle drain owns admission");
            }
            if let Some(lease) = &self.lease {
                // Retrying a lost acquire acknowledgement cannot extend its life.
                if lease.owner != owner
                    || epoch.checked_add(1) != Some(self.epoch)
                    || lease.committed
                {
                    return Err("another operation owns admission");
                }
            } else {
                if epoch != self.epoch {
                    return Err("stale drain epoch");
                }
                let seconds = request["wait_secs"]
                    .as_u64()
                    .filter(|s| (1..=MAX_WAIT_SECS).contains(s))
                    .ok_or("wait_secs must be from 1 through 7200")?;
                self.epoch = self.epoch.checked_add(1).ok_or("drain epoch exhausted")?;
                let deadline = now + Duration::from_secs(seconds);
                self.lease = Some(Lease {
                    owner: owner.to_owned(),
                    expires: (now + LEASE_TTL).min(deadline),
                    deadline,
                    committed: false,
                });
            }
        } else {
            let lease = self
                .lease
                .as_mut()
                .ok_or("drain lease expired or was released")?;
            if lease.owner != owner || epoch != self.epoch {
                return Err("drain owner or epoch differs");
            }
            match action {
                "inspect" => {}
                "renew" if !lease.committed => {
                    lease.expires = (now + LEASE_TTL).min(lease.deadline);
                }
                "release" if !lease.committed => self.lease = None,
                "commit" if lease.committed => {} // Lost acknowledgement is safe to retry.
                "commit" => {
                    if active != 0 {
                        return Err("inference is still active; lease remains waiting");
                    }
                    if lease.expires.saturating_duration_since(now) < MIN_COMMIT_ROOM {
                        return Err("insufficient lease lifetime to commit the stop");
                    }
                    lease.committed = true;
                }
                _ => return Err("unsupported action or committed stop requires recovery"),
            }
        }
        let mut result = self.snapshot(now);
        result["active_inference"] = json!(active);
        result["owner"] = json!(owner);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(epoch: u64) -> Value {
        json!({"owner":"0123456789abcdef","epoch":epoch,"wait_secs":600})
    }
    #[test]
    fn busy_wait_fences_new_work_and_only_commits_after_completion() {
        let mut a = Admission::default();
        let now = Instant::now();
        assert_eq!(
            a.control("acquire", &request(0), 2, now).unwrap()["phase"],
            "waiting"
        );
        assert!(a.blocked(now));
        assert!(a.control("commit", &request(1), 1, now).is_err());
        assert_eq!(
            a.control("commit", &request(1), 0, now).unwrap()["phase"],
            "committed"
        );
        assert!(a.blocked(now + Duration::from_secs(9999)));
        assert!(a.control("release", &request(1), 0, now).is_err());
        assert!(a.legacy(false, 0, now).is_err());
        assert!(a.control("commit", &request(1), 0, now).is_ok());
    }
    #[test]
    fn expiration_reopens_without_controller_and_prevents_aba() {
        let mut a = Admission::default();
        let now = Instant::now();
        a.control("acquire", &request(0), 1, now).unwrap();
        let later = now + LEASE_TTL;
        assert!(!a.blocked(later));
        assert!(a.control("acquire", &request(0), 0, later).is_err());
        assert!(a.control("renew", &request(1), 0, later).is_err());
        a.control("acquire", &request(1), 0, later).unwrap();
        for action in ["commit", "release", "renew", "inspect"] {
            assert!(a.control(action, &request(1), 0, later).is_err());
        }
        let mut wrong = request(2);
        wrong["owner"] = json!("fedcba9876543210");
        assert!(a.control("release", &wrong, 0, later).is_err());
        assert!(a.blocked(later));
    }
    #[test]
    fn renewal_is_bounded_and_near_expiry_cannot_commit() {
        let mut a = Admission::default();
        let now = Instant::now();
        let mut acquire = request(0);
        acquire["wait_secs"] = json!(31);
        a.control("acquire", &acquire, 0, now).unwrap();
        let late = now + Duration::from_secs(29);
        a.control("renew", &request(1), 0, late).unwrap();
        assert_eq!(a.snapshot(late)["remaining_ms"], 2000);
        assert!(a.control("commit", &request(1), 0, late).is_err());
        assert!(!a.blocked(now + Duration::from_secs(31)));
    }
    #[test]
    fn legacy_control_and_lease_control_cannot_steal_admission() {
        let mut a = Admission::default();
        let now = Instant::now();
        assert!(a.legacy(true, 1, now).is_err());
        assert!(!a.blocked(now));
        a.legacy(true, 0, now).unwrap();
        assert!(a.control("acquire", &request(0), 0, now).is_err());
        a.legacy(false, 0, now).unwrap();
        a.control("acquire", &request(0), 0, now).unwrap();
        assert!(a.legacy(false, 0, now).is_err());
        assert!(a.legacy(true, 0, now).is_err());
        assert!(a.snapshot(now).get("owner").is_none());
        a.control("release", &request(1), 0, now).unwrap();
        assert!(!a.blocked(now));
    }
    #[test]
    fn acquire_retry_does_not_extend_wait_and_invalid_operations_do_not_mutate() {
        let mut a = Admission::default();
        let now = Instant::now();
        let mut invalid = request(0);
        invalid["wait_secs"] = json!(7201);
        assert!(a.control("acquire", &invalid, 0, now).is_err());
        assert!(!a.blocked(now));
        a.control("acquire", &request(0), 0, now).unwrap();
        let later = now + Duration::from_secs(20);
        assert_eq!(
            a.control("acquire", &request(0), 0, later).unwrap()["remaining_ms"],
            10_000
        );
        assert!(a.control("unknown", &request(1), 0, later).is_err());
        assert!(a.blocked(later));
    }
}
