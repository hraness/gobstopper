use anyhow::{ensure, Context};
use base64::Engine;
use serde_json::{Map, Value};

pub(crate) const MAX_BODY_BYTES: usize = 13 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOTAL_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PIXELS: u64 = 16_000_000;

pub(crate) fn endpoint(account: &str, model: &str) -> anyhow::Result<String> {
    ensure!(
        account.len() == 32
            && account.bytes().all(|c| c.is_ascii_hexdigit())
            && matches!(model, "clef" | "clef-flash"),
        "clef_config_invalid"
    );
    Ok(format!(
        "https://api.cloudflare.com/client/v4/accounts/{account}/ai/run/@cf/cloudflare/{model}"
    ))
}

pub(crate) fn validate_endpoint(url: &str) -> anyhow::Result<()> {
    let parts = url
        .strip_prefix("https://api.cloudflare.com/client/v4/accounts/")
        .and_then(|suffix| suffix.split_once("/ai/run/@cf/cloudflare/"))
        .context("clef_config_invalid")?;
    ensure!(endpoint(parts.0, parts.1)? == url, "clef_config_invalid");
    Ok(())
}

fn evidence(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Object(_) | Value::Array(_) => true,
        _ => false,
    }
}

pub(crate) fn validate_request(request: &Value) -> anyhow::Result<()> {
    let error = "clef_request_invalid";
    let object = request.as_object().context(error)?;
    ensure!(
        object
            .keys()
            .all(|key| matches!(key.as_str(), "model" | "state" | "questions" | "images")),
        "{error}"
    );
    ensure!(
        matches!(request["model"].as_str(), Some("clef" | "clef-flash"))
            && evidence(&request["state"]),
        "{error}"
    );
    ensure!(
        serde_json::to_vec(request)
            .map_err(|_| anyhow::anyhow!(error))?
            .len()
            <= MAX_BODY_BYTES,
        "{error}"
    );
    let questions = request["questions"].as_object().context(error)?;
    ensure!((1..=64).contains(&questions.len()), "{error}");
    for (id, question) in questions {
        ensure!(
            (1..=100).contains(&id.len())
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
            "{error}"
        );
        let fields = question.as_object().context(error)?;
        ensure!(
            fields
                .keys()
                .all(|key| matches!(key.as_str(), "type" | "instructions" | "criteria"))
                && evidence(&question["instructions"]),
            "{error}"
        );
        match question["type"].as_str() {
            Some("noul") => {
                if let Some(criteria) = question.get("criteria") {
                    ensure!(
                        criteria.as_object().is_some_and(|c| c
                            .keys()
                            .all(|key| matches!(key.as_str(), "true" | "false"))),
                        "{error}"
                    );
                }
            }
            Some("choice") => ensure!(
                question["criteria"]
                    .as_object()
                    .is_some_and(|c| (2..=255).contains(&c.len())
                        && c.keys().all(|key| !key.is_empty() && key.len() <= 100)),
                "{error}"
            ),
            Some("score") => ensure!(
                question["criteria"]
                    .as_array()
                    .is_some_and(|c| (2..=10).contains(&c.len()) && c.iter().all(evidence)),
                "{error}"
            ),
            _ => anyhow::bail!("{error}"),
        }
    }
    if let Some(images) = request.get("images") {
        let images = images.as_array().context(error)?;
        ensure!(images.len() <= 4, "{error}");
        let mut total_image_bytes = 0usize;
        for image in images {
            let image_bytes = validate_image(image).map_err(|_| anyhow::anyhow!(error))?;
            total_image_bytes = total_image_bytes.checked_add(image_bytes).context(error)?;
            ensure!(total_image_bytes <= MAX_TOTAL_IMAGE_BYTES, "{error}");
        }
    }
    Ok(())
}

fn validate_image(value: &Value) -> anyhow::Result<usize> {
    let error = "clef_image_invalid";
    let (kind, encoded) = if let Some(url) = value.as_str() {
        let (prefix, encoded) = url.split_once(',').context(error)?;
        let kind = match prefix.to_ascii_lowercase().as_str() {
            "data:image/png;base64" => "image/png",
            "data:image/jpeg;base64" => "image/jpeg",
            "data:image/webp;base64" => "image/webp",
            _ => anyhow::bail!("{error}"),
        };
        (kind, encoded)
    } else {
        let object = value.as_object().context(error)?;
        ensure!(
            object.len() == 2
                && object
                    .keys()
                    .all(|key| matches!(key.as_str(), "content_type" | "base64")),
            "{error}"
        );
        (
            value["content_type"].as_str().context(error)?,
            value["base64"].as_str().context(error)?,
        )
    };
    let format = match kind {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        _ => anyhow::bail!("{error}"),
    };
    ensure!(
        !encoded.is_empty() && encoded.len() <= MAX_IMAGE_BYTES.div_ceil(3) * 4,
        "{error}"
    );
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!(error))?;
    ensure!(
        !bytes.is_empty()
            && bytes.len() <= MAX_IMAGE_BYTES
            && image::guess_format(&bytes).ok() == Some(format),
        "{error}"
    );
    let reader = || image::ImageReader::with_format(std::io::Cursor::new(&bytes), format);
    let (width, height) = reader()
        .into_dimensions()
        .map_err(|_| anyhow::anyhow!(error))?;
    ensure!(
        width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_PIXELS,
        "{error}"
    );
    let mut decoder = reader();
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    decoder.limits(limits);
    decoder.decode().map_err(|_| anyhow::anyhow!(error))?;
    Ok(bytes.len())
}

pub(crate) fn read_evidence(path: &std::path::Path) -> anyhow::Result<Value> {
    use std::io::Read;
    let error = "clef_evidence_invalid";
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| anyhow::anyhow!(error))?;
    let metadata = file.metadata().map_err(|_| anyhow::anyhow!(error))?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_BODY_BYTES as u64,
        "{error}"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!(error))?;
    ensure!(bytes.len() <= MAX_BODY_BYTES, "{error}");
    let mut value = crate::mcp::strict_json(&bytes).map_err(|_| anyhow::anyhow!(error))?;
    let object = value.as_object_mut().context(error)?;
    object.entry("model").or_insert_with(|| "clef".into());
    validate_request(&value)?;
    Ok(value)
}

fn probability(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
}

const CLOUDFLARE_ROUNDING_RADIUS: f64 = 0.00005;

fn probability_interval(reported: f64) -> (f64, f64) {
    (
        (reported - CLOUDFLARE_ROUNDING_RADIUS).max(0.0),
        (reported + CLOUDFLARE_ROUNDING_RADIUS).min(1.0),
    )
}

fn arithmetic_slack(size: usize) -> f64 {
    64.0 * f64::EPSILON * size as f64
}

fn distribution<'a>(answer: &'a Value, keys: &[String]) -> anyhow::Result<&'a Map<String, Value>> {
    let error = "clef_response_invalid";
    let probabilities = answer["probabilities"].as_object().context(error)?;
    ensure!(
        probabilities.len() == keys.len()
            && keys
                .iter()
                .all(|key| probabilities.get(key).and_then(probability).is_some()),
        "{error}"
    );
    let (lower, upper) = probabilities
        .values()
        .filter_map(probability)
        .map(probability_interval)
        .fold((0.0, 0.0), |(low, high), (l, h)| (low + l, high + h));
    let slack = arithmetic_slack(keys.len());
    ensure!(
        lower <= 1.0 + slack
            && upper >= 1.0 - slack
            && probability(&answer["confidence"]).is_some(),
        "{error}"
    );
    Ok(probabilities)
}

fn feasible_score_endpoint(intervals: &[(f64, f64)], highest: bool) -> f64 {
    let mut remaining = (1.0 - intervals.iter().map(|(low, _)| low).sum::<f64>()).max(0.0);
    let mut score: f64 = intervals
        .iter()
        .enumerate()
        .map(|(rank, (low, _))| rank as f64 * low)
        .sum();
    for position in 0..intervals.len() {
        let rank = if highest {
            intervals.len() - 1 - position
        } else {
            position
        };
        let (low, high) = intervals[rank];
        let allocated = remaining.min(high - low);
        score += rank as f64 * allocated;
        remaining = (remaining - allocated).max(0.0);
    }
    score
}

fn score_is_feasible(probabilities: &Map<String, Value>, keys: &[String], score: &Value) -> bool {
    let Some(reported) = score
        .as_f64()
        .filter(|score| score.is_finite() && (0.0..=(keys.len() - 1) as f64).contains(score))
    else {
        return false;
    };
    let intervals: Vec<_> = keys
        .iter()
        .map(|key| probability_interval(probability(&probabilities[key]).unwrap_or(0.0)))
        .collect();
    let minimum = feasible_score_endpoint(&intervals, false);
    let maximum = feasible_score_endpoint(&intervals, true);
    let slack = arithmetic_slack(keys.len()) * keys.len() as f64;
    reported + CLOUDFLARE_ROUNDING_RADIUS >= minimum - slack
        && reported - CLOUDFLARE_ROUNDING_RADIUS <= maximum + slack
}

pub(crate) fn validate_response(request: &Value, envelope: &Value) -> anyhow::Result<Value> {
    let error = "clef_response_invalid";
    ensure!(
        envelope["success"].as_bool() == Some(true)
            && envelope
                .get("errors")
                .is_none_or(|errors| errors.as_array().is_some_and(Vec::is_empty)),
        "{error}"
    );
    let result = envelope.get("result").context(error)?;
    ensure!(
        result.as_object().is_some_and(|r| r
            .keys()
            .all(|key| matches!(key.as_str(), "model" | "answers" | "usage")))
            && result["model"] == request["model"],
        "{error}"
    );
    let usage = result["usage"].as_object().context(error)?;
    ensure!(
        usage.len() == 2
            && ["input_tokens", "output_tokens"]
                .iter()
                .all(|key| usage.get(*key).and_then(Value::as_u64).is_some()),
        "{error}"
    );
    let questions = request["questions"].as_object().context(error)?;
    let answers = result["answers"].as_object().context(error)?;
    ensure!(answers.len() <= questions.len(), "{error}");
    for (id, answer) in answers {
        let question = questions.get(id).context(error)?;
        ensure!(answer["type"] == question["type"], "{error}");
        match question["type"].as_str() {
            Some("noul") => ensure!(
                probability(&answer["noul"]).is_some()
                    && answer.as_object().is_some_and(|a| a.len() == 2),
                "{error}"
            ),
            Some("choice") => {
                ensure!(answer.as_object().is_some_and(|a| a.len() == 4), "{error}");
                let keys: Vec<String> = question["criteria"]
                    .as_object()
                    .context(error)?
                    .keys()
                    .cloned()
                    .collect();
                let probabilities = distribution(answer, &keys)?;
                let chosen = answer["choice"].as_str().context(error)?;
                let best = probabilities
                    .get(chosen)
                    .and_then(probability)
                    .context(error)?;
                ensure!(
                    probabilities
                        .values()
                        .filter_map(probability)
                        .all(|p| p <= best),
                    "{error}"
                );
            }
            Some("score") => {
                ensure!(answer.as_object().is_some_and(|a| a.len() == 5), "{error}");
                let levels = question["criteria"].as_array().context(error)?;
                let keys: Vec<String> = (0..levels.len()).map(|index| index.to_string()).collect();
                let probabilities = distribution(answer, &keys)?;
                let legend = answer["legend"].as_object().context(error)?;
                ensure!(
                    legend.len() == levels.len()
                        && keys
                            .iter()
                            .enumerate()
                            .all(|(index, key)| legend.get(key) == Some(&levels[index])),
                    "{error}"
                );
                ensure!(
                    score_is_feasible(probabilities, &keys, &answer["score"]),
                    "{error}"
                );
            }
            _ => anyhow::bail!("{error}"),
        }
    }
    Ok(result.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> serde_json::Value {
        json!({"model":"clef","state":{"evidence":"explicit only"},"questions":{"keep":{"type":"noul","instructions":"Keep this?"}}})
    }

    fn response() -> serde_json::Value {
        json!({"success":true,"errors":[],"result":{"model":"clef","answers":{"keep":{"type":"noul","noul":0.8}},"usage":{"input_tokens":4,"output_tokens":0}}})
    }

    #[test]
    fn endpoint_is_fixed_and_rejects_account_and_model_injection() {
        assert_eq!(endpoint("0123456789abcdef0123456789abcdef", "clef").unwrap(), "https://api.cloudflare.com/client/v4/accounts/0123456789abcdef0123456789abcdef/ai/run/@cf/cloudflare/clef");
        for account in [
            "",
            "../evil",
            "0123456789abcdef0123456789abcdef?x",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert!(endpoint(account, "clef").is_err());
        }
        assert!(endpoint("0123456789abcdef0123456789abcdef", "jev-latest").is_err());
        assert!(endpoint("0123456789abcdef0123456789abcdef", "clef-flash")
            .unwrap()
            .ends_with("/clef-flash"));
    }

    #[test]
    fn request_requires_ids_instructions_choices_and_body_bound() {
        validate_request(&request()).unwrap();
        for (id, question) in [
            ("bad/id", json!({"type":"noul","instructions":"x"})),
            ("x", json!({"type":"noul"})),
            ("x", json!({"type":"noul","instructions":" "})),
            (
                "x",
                json!({"type":"choice","instructions":"x","criteria":{"only":"one"}}),
            ),
            (
                "x",
                json!({"type":"score","instructions":"x","criteria":["one"]}),
            ),
        ] {
            let mut value = request();
            value["questions"] = json!({id:question});
            assert!(validate_request(&value).is_err());
        }
        let mut value = request();
        value["state"] = "x".repeat(MAX_BODY_BYTES).into();
        assert!(validate_request(&value).is_err());
    }

    #[test]
    fn rest_envelope_model_types_and_identity_are_required_and_errors_private() {
        validate_response(&request(), &response()).unwrap();
        for change in [
            json!({"success":false,"errors":[{"message":"SENSITIVE"}],"result":null}),
            json!({"answers":{"keep":0.8}}),
        ] {
            let error = validate_response(&request(), &change).unwrap_err();
            assert_eq!(error.to_string(), "clef_response_invalid");
        }
        for result in [
            json!({"model":"clef-flash","answers":{"keep":{"type":"noul","noul":0.8}}}),
            json!({"model":"clef","answers":{"other":{"type":"noul","noul":0.8}}}),
            json!({"model":"clef","answers":{"keep":{"type":"noul","noul":1.8}}}),
            json!({"model":"clef","answers":{"keep":{"type":"score","score":0.8}}}),
        ] {
            let mut value = response();
            value["result"] = result;
            value["result"]["usage"] = response()["result"]["usage"].clone();
            assert!(validate_response(&request(), &value).is_err());
        }
    }

    #[test]
    fn choice_and_score_must_match_requested_options_probabilities_and_legend() {
        let mut req = request();
        req["questions"] = json!({"team":{"type":"choice","instructions":"Which?","criteria":{"a":"A","b":"B"}},"impact":{"type":"score","instructions":"How much?","criteria":["low","high"]}});
        let mut res = response();
        res["result"]["answers"] = json!({"team":{"type":"choice","choice":"b","probabilities":{"a":0.2,"b":0.8},"confidence":0.8},"impact":{"type":"score","score":0.75,"legend":{"0":"low","1":"high"},"probabilities":{"0":0.25,"1":0.75},"confidence":0.75}});
        validate_response(&req, &res).unwrap();
        for (field, bad) in [
            ("choice", json!("a")),
            ("probabilities", json!({"a":0.2,"foreign":0.8})),
            ("probabilities", json!({"a":0.2,"b":0.2})),
        ] {
            let mut value = res.clone();
            value["result"]["answers"]["team"][field] = bad;
            assert!(validate_response(&req, &value).is_err());
        }
        res["result"]["answers"]["impact"]["score"] = 0.1.into();
        assert!(validate_response(&req, &res).is_err());
    }

    fn score_case(model: &str, probabilities: &[f64], score: f64) -> (Value, Value) {
        let levels: Vec<_> = (0..probabilities.len())
            .map(|i| format!("level {i}"))
            .collect();
        let legend: Map<_, _> = levels
            .iter()
            .enumerate()
            .map(|(i, level)| (i.to_string(), json!(level)))
            .collect();
        let probabilities: Map<_, _> = probabilities
            .iter()
            .enumerate()
            .map(|(i, p)| (i.to_string(), json!(p)))
            .collect();
        let req = json!({"model":model,"state":"synthetic","questions":{"severity":{"type":"score","instructions":"Impact?","criteria":levels}}});
        let res = json!({"success":true,"errors":[],"result":{"model":model,"answers":{"severity":{"type":"score","score":score,"legend":legend,"probabilities":probabilities,"confidence":0.8}},"usage":{"input_tokens":319,"output_tokens":0}}});
        (req, res)
    }

    #[test]
    fn recorded_cloudflare_models_accept_independently_rounded_scores_verbatim() {
        for (model, probabilities, score, confidence, urgent, technical, sales, team_confidence) in [
            (
                "clef",
                [0.0047, 0.0051, 0.0448, 0.9454],
                2.931,
                0.8612,
                0.9869,
                0.9635,
                0.0365,
                0.8593,
            ),
            (
                "clef-flash",
                [0.0149, 0.0157, 0.186, 0.7834],
                2.7378,
                0.5316,
                0.9354,
                0.9724,
                0.0276,
                0.8928,
            ),
        ] {
            let (mut req, mut res) = score_case(model, &probabilities, score);
            req["questions"]["severity"]["criteria"] =
                json!(["No impact", "Minor", "Major", "Critical"]);
            req["questions"]["urgent"] = json!({"type":"noul","instructions":"Urgent?"});
            req["questions"]["team"] = json!({"type":"choice","instructions":"Which team?","criteria":{"technical":"Technical","sales":"Sales"}});
            res["result"]["answers"]["severity"]["legend"] =
                json!({"0":"No impact","1":"Minor","2":"Major","3":"Critical"});
            res["result"]["answers"]["severity"]["confidence"] = json!(confidence);
            res["result"]["answers"]["urgent"] = json!({"type":"noul","noul":urgent});
            res["result"]["answers"]["team"] = json!({"type":"choice","choice":"technical","probabilities":{"technical":technical,"sales":sales},"confidence":team_confidence});
            assert_eq!(validate_response(&req, &res).unwrap(), res["result"]);
            for delta in [-0.01, 0.01] {
                res["result"]["answers"]["severity"]["score"] = json!(score + delta);
                assert!(validate_response(&req, &res).is_err());
            }
        }
    }

    #[test]
    fn rounded_distribution_requires_normalized_mass_and_clipped_score_feasibility() {
        for (probabilities, score) in [
            (vec![0.3333, 0.3333, 0.3333], 1.0),
            (vec![0.3334, 0.3333, 0.3334], 1.0),
            (vec![1.0, 0.0], 0.0001),
            (vec![0.0, 1.0], 0.9999),
        ] {
            let (req, res) = score_case("clef", &probabilities, score);
            assert_eq!(validate_response(&req, &res).unwrap(), res["result"]);
        }
        for (probabilities, score) in [
            (vec![0.3334; 3], 1.0),
            (vec![0.3332; 3], 1.0),
            (vec![1.0, 0.0], 0.0002),
            (vec![0.0, 1.0], 0.9998),
            (vec![1.0, 0.0], -0.00001),
            (vec![0.0, 1.0], 1.00001),
            (vec![0.5, 0.5], 0.5002),
            (vec![-0.00001, 1.0], 1.0),
            (vec![0.0, 1.00001], 1.0),
        ] {
            let (req, res) = score_case("clef", &probabilities, score);
            assert!(
                validate_response(&req, &res).is_err(),
                "accepted {probabilities:?}, {score}"
            );
        }
    }

    #[test]
    fn normalized_distributions_and_scores_survive_four_decimal_round_trips() {
        let round = |value: f64| (value * 10_000.0).round() / 10_000.0;
        let mut seed = 1u64;
        for size in 2..=10 {
            for _ in 0..64 {
                let weights: Vec<_> = (0..size)
                    .map(|_| {
                        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                        (seed % 101) as f64
                    })
                    .collect();
                let total: f64 = weights.iter().sum();
                let normalized: Vec<_> = weights.iter().map(|weight| weight / total).collect();
                let score = round(
                    normalized
                        .iter()
                        .enumerate()
                        .map(|(rank, p)| rank as f64 * p)
                        .sum(),
                );
                let reported: Vec<_> = normalized.into_iter().map(round).collect();
                let (req, mut res) = score_case("clef", &reported, score);
                assert_eq!(validate_response(&req, &res).unwrap(), res["result"]);
                for delta in [-0.01, 0.01] {
                    res["result"]["answers"]["severity"]["score"] = json!(score + delta);
                    assert!(validate_response(&req, &res).is_err());
                }
            }
        }
    }

    #[test]
    fn rounded_choice_still_requires_the_highest_reported_probability() {
        let req = json!({"model":"clef","state":"synthetic","questions":{"team":{"type":"choice","instructions":"Which?","criteria":{"a":"A","b":"B","c":"C"}}}});
        let mut res = response();
        res["result"]["answers"] = json!({"team":{"type":"choice","choice":"a","probabilities":{"a":0.3334,"b":0.3333,"c":0.3334},"confidence":0.8}});
        validate_response(&req, &res).unwrap();
        res["result"]["answers"]["team"]["choice"] = json!("b");
        assert!(validate_response(&req, &res).is_err());
        res["result"]["answers"]["team"] = json!({"type":"choice","choice":"b","probabilities":{"a":0.333334,"b":0.333333,"c":0.333333},"confidence":0.8});
        assert!(validate_response(&req, &res).is_err());
    }

    fn crc(bytes: &[u8]) -> u32 {
        let mut value = u32::MAX;
        for byte in bytes {
            value ^= u32::from(*byte);
            for _ in 0..8 {
                value = (value >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(value & 1)));
            }
        }
        !value
    }

    #[test]
    fn image_byte_total_counts_embedded_files_not_decoded_raster_pixels() {
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1500, 1500)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        let bytes = output.into_inner();
        assert!(bytes.len() < MAX_IMAGE_BYTES);
        let embedded = json!({"content_type":"image/png","base64":base64::engine::general_purpose::STANDARD.encode(&bytes)});
        let mut value = request();
        value["images"] = json!(vec![embedded; 4]);
        validate_request(&value).unwrap();
    }

    #[test]
    fn clef_image_byte_total_pixel_and_decoder_limits() {
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        let png = output.into_inner();
        let embedded = |bytes: &[u8]| json!({"content_type":"image/png","base64":base64::engine::general_purpose::STANDARD.encode(bytes)});
        let mut value = request();
        let mut oversized = png.clone();
        oversized.resize(MAX_IMAGE_BYTES + 1, 0);
        value["images"] = json!([embedded(&oversized)]);
        assert!(validate_request(&value).is_err());
        let mut large_dimensions = png.clone();
        large_dimensions[16..20].copy_from_slice(&4001u32.to_be_bytes());
        large_dimensions[20..24].copy_from_slice(&4001u32.to_be_bytes());
        let checksum = crc(&large_dimensions[12..29]);
        large_dimensions[29..33].copy_from_slice(&checksum.to_be_bytes());
        value["images"] = json!([embedded(&large_dimensions)]);
        assert!(validate_request(&value).is_err());
        value["images"] = json!([embedded(&png[..png.len() / 2])]);
        assert!(validate_request(&value).is_err());
        let mut metadata = b"note\0".to_vec();
        metadata.resize(3 * 1024 * 1024, b'x');
        let mut chunk = (metadata.len() as u32).to_be_bytes().to_vec();
        chunk.extend_from_slice(b"tEXt");
        chunk.extend_from_slice(&metadata);
        chunk.extend_from_slice(&crc(&chunk[4..]).to_be_bytes());
        let mut large = png[..33].to_vec();
        large.extend_from_slice(&chunk);
        large.extend_from_slice(&png[33..]);
        value["images"] = json!([embedded(&large), embedded(&large)]);
        validate_request(&value).unwrap();
        value["images"] = json!([embedded(&large), embedded(&large), embedded(&large)]);
        assert!(validate_request(&value).is_err());
        for (format, kind) in [
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::WebP, "image/webp"),
        ] {
            let mut output = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(1, 1)
                .write_to(&mut output, format)
                .unwrap();
            value["images"] = json!([{"content_type":kind,"base64":base64::engine::general_purpose::STANDARD.encode(output.into_inner())}]);
            validate_request(&value).unwrap();
        }
    }

    #[test]
    fn images_are_explicit_embedded_valid_and_bounded() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let png = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
        let mut value = request();
        value["images"] = json!([{"content_type":"image/png","base64":png}]);
        validate_request(&value).unwrap();
        for images in [
            json!(["https://example.com/private.png"]),
            json!(["data:image/svg+xml;base64,PHN2Zz4="]),
            json!([{"content_type":"image/png","base64":"!!!!"}]),
            json!([{"content_type":"image/jpeg","base64":png}]),
            json!([{"content_type":"image/png","base64":"AAAA"}]),
            json!(vec![format!("data:image/png;base64,{png}"); 5]),
        ] {
            value["images"] = images;
            assert!(validate_request(&value).is_err());
        }
        assert!(!request().as_object().unwrap().contains_key("images"));
    }
}
