use anyhow::{bail, Result};

#[derive(Debug)]
pub(crate) struct DependentCaller(pub &'static str);

impl std::fmt::Display for DependentCaller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} indicates this caller depends on the proxy. Run this command from a terminal outside an agent tool shell, or pass --allow-dependent-caller.", self.0)
    }
}

impl std::error::Error for DependentCaller {}

fn numeric_loopback(host: &str) -> bool {
    let parts = host.trim_end_matches('.').split('.').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 4 {
        return false;
    }
    let mut numbers = Vec::new();
    for part in &parts {
        let (digits, radix) = if part.starts_with("0x") || part.starts_with("0X") {
            (&part[2..], 16)
        } else if part.len() > 1 && part.starts_with('0') {
            (&part[1..], 8)
        } else {
            (*part, 10)
        };
        let Ok(number) = u64::from_str_radix(digits, radix) else {
            return false;
        };
        numbers.push(number);
    }
    let tail_bits = 8 * (5 - numbers.len());
    if numbers[..numbers.len() - 1]
        .iter()
        .any(|number| *number > 255)
        || numbers[numbers.len() - 1] >= (1u64 << tail_bits)
    {
        return false;
    }
    let mut address = numbers[numbers.len() - 1];
    for (index, number) in numbers[..numbers.len() - 1].iter().enumerate() {
        address |= number << (8 * (3 - index));
    }
    address == u32::from(std::net::Ipv4Addr::LOCALHOST) as u64
}

fn loopback_service_url(value: &str, port: u16) -> bool {
    let Some((scheme, rest)) = value.trim().split_once("://") else {
        return false;
    };
    let default_port = if scheme.eq_ignore_ascii_case("http") {
        80
    } else if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, actual_port) = if authority.starts_with('[') {
        let Some(end) = authority.find(']') else {
            return false;
        };
        let suffix = &authority[end + 1..];
        let actual_port = if suffix.is_empty() {
            default_port
        } else if let Some(port) = suffix.strip_prefix(':') {
            match port.parse::<u16>() {
                Ok(port) => port,
                Err(_) => return false,
            }
        } else {
            return false;
        };
        (&authority[..=end], actual_port)
    } else if let Some((host, explicit)) = authority.rsplit_once(':') {
        let Ok(port) = explicit.parse::<u16>() else {
            return false;
        };
        (host, port)
    } else {
        (authority, default_port)
    };
    if actual_port != port || port == 0 || host.len() > 255 {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("localhost.")
        || numeric_loopback(host)
    {
        return true;
    }
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .and_then(|host| host.parse::<std::net::Ipv6Addr>().ok())
        .is_some_and(|address| {
            address.is_loopback() || address.to_ipv4() == Some(std::net::Ipv4Addr::LOCALHOST)
        })
}

fn canonical_signal_key(key: &str) -> Option<&'static str> {
    [
        "ANTHROPIC_BASE_URL",
        "OPENAI_BASE_URL",
        "GOBSTOPPER_SCOPE",
        "CLAUDECODE",
        "ANTHROPIC_CUSTOM_HEADERS",
    ]
    .into_iter()
    .find(|name| name.eq_ignore_ascii_case(key))
}

fn dependent_signal(port: u16, env: &[(String, String)]) -> Option<&'static str> {
    for (key, value) in env {
        let Some(key) = canonical_signal_key(key) else {
            continue;
        };
        if matches!(key, "ANTHROPIC_BASE_URL" | "OPENAI_BASE_URL")
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
    if allow {
        return Ok(());
    }
    let mut env = Vec::new();
    for (key, value) in std::env::vars_os() {
        let Some(key) = key.to_str().and_then(canonical_signal_key) else {
            continue;
        };
        let Some(value) = value.to_str() else {
            bail!(DependentCaller(key));
        };
        env.push((key.to_owned(), value.to_owned()));
    }
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

    #[test]
    fn normalized_loopback_aliases_and_case_insensitive_keys_are_guarded() {
        for url in [
            "http://127.1:8260/v1",
            "http://2130706433:08260/v1",
            "http://0x7f000001:8260/v1",
            "http://0177.0.0.1:8260/v1",
            "http://[0:0:0:0:0:0:0:1]:8260/v1",
            "http://[::ffff:127.0.0.1]:8260/v1",
            "http://user:password@localhost:8260/v1",
        ] {
            assert!(loopback_service_url(url, 8260), "{url}");
            assert_eq!(
                dependent_signal(8260, &[("openai_base_url".into(), url.into())]),
                Some("OPENAI_BASE_URL")
            );
            assert!(!loopback_service_url(url, 8261));
        }
        assert!(loopback_service_url("http://localhost/v1", 80));
        assert!(loopback_service_url("https://localhost/v1", 443));
        for url in [
            "http://127.0.0.2:8260/v1",
            "http://localhost.example:8260/v1",
            "http://localhost:8260@foreign.example/v1",
            "http://127.999:8260/v1",
            "http://localhost:8260extra/v1",
        ] {
            assert!(!loopback_service_url(url, 8260), "{url}");
        }
    }

    #[test]
    fn loopback_url_spelling_and_authority_delimiters() {
        for url in [
            "HTTP://LOCALHOST:8260?query=1",
            "HtTpS://localhost.:8260#fragment",
            "http://[::1]:8260?query=1",
            "https://127.0.0.1:8260/#fragment",
        ] {
            assert!(loopback_service_url(url, 8260), "{url}");
            assert!(!loopback_service_url(url, 8261), "{url}");
        }
    }
}
