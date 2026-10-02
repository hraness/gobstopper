use anyhow::{bail, Result};

#[derive(Debug)]
pub(crate) struct DependentCaller(pub &'static str);

impl std::fmt::Display for DependentCaller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} indicates this caller depends on the proxy. Run gobstopper proxy upgrade for an upgrade, run this command from a terminal outside an agent tool shell, or pass --allow-dependent-caller.", self.0)
    }
}

impl std::error::Error for DependentCaller {}

fn loopback_service_url(value: &str, port: u16) -> bool {
    let Some(authority) = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .and_then(|rest| rest.split('/').next())
    else {
        return false;
    };
    let expected_port = port.to_string();
    let Some(host) = authority.strip_suffix(&format!(":{expected_port}")) else {
        return false;
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    )
}

fn dependent_signal(port: u16, env: &[(String, String)]) -> Option<&'static str> {
    for (key, value) in env {
        if matches!(key.as_str(), "ANTHROPIC_BASE_URL" | "OPENAI_BASE_URL")
            && loopback_service_url(value, port)
        {
            return Some(if key == "ANTHROPIC_BASE_URL" {
                "ANTHROPIC_BASE_URL"
            } else {
                "OPENAI_BASE_URL"
            });
        }
        if key == "GOBSTOPPER_SCOPE" || key == "CLAUDECODE" {
            return Some(if key == "GOBSTOPPER_SCOPE" {
                "GOBSTOPPER_SCOPE"
            } else {
                "CLAUDECODE"
            });
        }
        if key == "ANTHROPIC_CUSTOM_HEADERS"
            && value.lines().any(|line| {
                line.split_once(':')
                    .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("X-Gobstopper-Scope"))
            })
        {
            return Some("ANTHROPIC_CUSTOM_HEADERS: X-Gobstopper-Scope");
        }
    }
    None
}

fn refusal_signal(
    port: u16,
    allow: bool,
    print: bool,
    env: &[(String, String)],
) -> Option<&'static str> {
    if allow || print {
        None
    } else {
        dependent_signal(port, env)
    }
}

pub(crate) fn refuse(port: u16, allow: bool) -> Result<()> {
    let env = std::env::vars().collect::<Vec<_>>();
    if let Some(signal) = refusal_signal(port, allow, false, &env) {
        bail!(DependentCaller(signal));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_signal_and_other_port() {
        for (key, value) in [
            (
                "ANTHROPIC_BASE_URL",
                "http://127.0.0.1:8260/__gobstopper/s/abc",
            ),
            ("OPENAI_BASE_URL", "http://localhost:8260/v1"),
            ("OPENAI_BASE_URL", "http://[::1]:8260/v1"),
            ("GOBSTOPPER_SCOPE", "abc"),
            (
                "ANTHROPIC_CUSTOM_HEADERS",
                "foo: bar\nx-gObStOpPeR-sCoPe: abc",
            ),
            ("CLAUDECODE", "1"),
        ] {
            assert!(dependent_signal(8260, &[(key.into(), value.into())]).is_some());
        }
        assert_eq!(
            dependent_signal(
                8260,
                &[("OPENAI_BASE_URL".into(), "http://127.0.0.1:8261/v1".into())]
            ),
            None
        );
    }

    #[test]
    fn allow_flag_and_print_skip_refusal() {
        let env = vec![("CLAUDECODE".into(), "1".into())];
        assert_eq!(refusal_signal(8260, true, false, &env), None);
        assert_eq!(refusal_signal(8260, false, true, &env), None);
        assert_eq!(refusal_signal(8260, false, false, &env), Some("CLAUDECODE"));
    }
}
