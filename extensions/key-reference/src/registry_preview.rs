//! Provider-specific, keyless registry lookups for external link cards.
//!
//! This module is intentionally small: it recognizes a canonical public URL,
//! constructs a fixed official API endpoint, then maps its JSON into the
//! shared link-card model. The reader never needs registry-specific network
//! code or JSON knowledge.

use crate::link_preview::{LinkPreviewKind, fetch_public_json};
use key_safe_http::CancellationToken;
use serde_json::Value;
use url::Url;

const MAX_REGISTRY_BYTES: usize = 512 * 1024;
const MAX_DETAIL_LINES: usize = 4;
const MAX_DETAIL_CHARS: usize = 180;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistryPreview {
    pub kind: LinkPreviewKind,
    pub title: String,
    pub details: Vec<String>,
    pub resolved_url: String,
}

/// Returns `None` for ordinary websites; recognized registry URLs receive a
/// bounded request to a fixed public API endpoint.
pub(crate) fn fetch_registry_preview(
    source_url: &str,
    cancellation: &CancellationToken,
) -> Option<Result<RegistryPreview, String>> {
    let source = Url::parse(source_url).ok()?;
    let host = source
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if matches!(
        host.as_str(),
        "clinicaltrials.gov" | "www.clinicaltrials.gov"
    ) {
        let nct_id = find_nct_id(source.as_str())?;
        return Some(fetch_clinical_trials(&nct_id, cancellation));
    }
    if matches!(host.as_str(), "osf.io" | "www.osf.io") {
        let registration_id = osf_registration_id(&source)?;
        return Some(fetch_osf_registration(&registration_id, cancellation));
    }
    None
}

fn fetch_clinical_trials(
    nct_id: &str,
    cancellation: &CancellationToken,
) -> Result<RegistryPreview, String> {
    let endpoint = format!("https://clinicaltrials.gov/api/v2/studies/{nct_id}");
    let body = fetch_public_json(&endpoint, MAX_REGISTRY_BYTES, cancellation)
        .map_err(|error| format!("ClinicalTrials.gov: {error}"))?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|error| format!("ClinicalTrials.gov returned invalid JSON: {error}"))?;
    parse_clinical_trials(&value, nct_id)
}

fn parse_clinical_trials(value: &Value, nct_id: &str) -> Result<RegistryPreview, String> {
    let title = string_at(&value, "/protocolSection/identificationModule/briefTitle")
        .or_else(|| {
            string_at(
                &value,
                "/protocolSection/identificationModule/officialTitle",
            )
        })
        .ok_or_else(|| "ClinicalTrials.gov returned no study title".to_owned())?;
    let mut details = vec![format!("Trial: {nct_id}")];
    push_field(
        &mut details,
        "Status",
        string_at(&value, "/protocolSection/statusModule/overallStatus"),
    );
    push_field(
        &mut details,
        "Type",
        string_at(&value, "/protocolSection/designModule/studyType"),
    );
    push_field(
        &mut details,
        "Condition",
        first_string_at(&value, "/protocolSection/conditionsModule/conditions"),
    );
    Ok(RegistryPreview {
        kind: LinkPreviewKind::ClinicalTrialsGov,
        title,
        details: normalize_details(details),
        resolved_url: format!("https://clinicaltrials.gov/study/{nct_id}"),
    })
}

fn fetch_osf_registration(
    registration_id: &str,
    cancellation: &CancellationToken,
) -> Result<RegistryPreview, String> {
    let endpoint = format!("https://api.osf.io/v2/registrations/{registration_id}/");
    let body = fetch_public_json(&endpoint, MAX_REGISTRY_BYTES, cancellation)
        .map_err(|error| format!("OSF: {error}"))?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|error| format!("OSF returned invalid JSON: {error}"))?;
    parse_osf_registration(&value, registration_id)
}

fn parse_osf_registration(value: &Value, registration_id: &str) -> Result<RegistryPreview, String> {
    let title = string_at(&value, "/data/attributes/title")
        .ok_or_else(|| "OSF returned no registration title".to_owned())?;
    let mut details = vec![format!("Registration: {registration_id}")];
    push_field(
        &mut details,
        "Registered",
        string_at(&value, "/data/attributes/date_registered"),
    );
    push_field(
        &mut details,
        "Provider",
        string_at(&value, "/data/attributes/provider"),
    );
    push_field(
        &mut details,
        "Description",
        string_at(&value, "/data/attributes/description"),
    );
    Ok(RegistryPreview {
        kind: LinkPreviewKind::Osf,
        title,
        details: normalize_details(details),
        resolved_url: format!("https://osf.io/{registration_id}/"),
    })
}

fn find_nct_id(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    for index in 0..bytes.len().saturating_sub(10) {
        if !bytes[index..].starts_with(b"NCT") && !bytes[index..].starts_with(b"nct") {
            continue;
        }
        let end = index + 11;
        if end > bytes.len()
            || (index > 0 && bytes[index - 1].is_ascii_alphanumeric())
            || (end < bytes.len() && bytes[end].is_ascii_alphanumeric())
            || !bytes[index + 3..end].iter().all(u8::is_ascii_digit)
        {
            continue;
        }
        return Some(value[index..end].to_ascii_uppercase());
    }
    None
}

fn osf_registration_id(url: &Url) -> Option<String> {
    let id = url.path_segments()?.next()?;
    (id.len() == 5 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .then(|| id.to_ascii_lowercase())
}

fn string_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(normalize_text)
        .filter(|value| !value.is_empty())
}

fn first_string_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)?
        .as_array()?
        .iter()
        .find_map(Value::as_str)
        .map(normalize_text)
        .filter(|value| !value.is_empty())
}

fn push_field(details: &mut Vec<String>, label: &str, value: Option<String>) {
    if let Some(value) = value {
        details.push(format!("{label}: {value}"));
    }
}

fn normalize_details(details: Vec<String>) -> Vec<String> {
    details
        .into_iter()
        .take(MAX_DETAIL_LINES)
        .map(|detail| {
            let mut result = normalize_text(&detail);
            if result.chars().count() > MAX_DETAIL_CHARS {
                result = result
                    .chars()
                    .take(MAX_DETAIL_CHARS - 1)
                    .collect::<String>();
                result.push('…');
            }
            result
        })
        .collect()
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognizes_registry_identifiers_without_overmatching() {
        assert_eq!(
            find_nct_id("https://clinicaltrials.gov/study/NCT01234567"),
            Some("NCT01234567".into())
        );
        assert_eq!(
            find_nct_id("https://example.test/NCT01234567"),
            Some("NCT01234567".into())
        );
        assert_eq!(find_nct_id("NCT012345678"), None);
        let osf = Url::parse("https://osf.io/ab12c/").unwrap();
        assert_eq!(osf_registration_id(&osf), Some("ab12c".into()));
        let project = Url::parse("https://osf.io/registrations/").unwrap();
        assert_eq!(osf_registration_id(&project), None);
    }

    #[test]
    fn maps_clinical_trials_json_to_bounded_card_fields() {
        let trial = json!({"protocolSection":{"identificationModule":{"briefTitle":"A Trial"},"statusModule":{"overallStatus":"RECRUITING"},"designModule":{"studyType":"INTERVENTIONAL"},"conditionsModule":{"conditions":["Condition A"]}}});
        let preview = parse_clinical_trials(&trial, "NCT01234567").unwrap();
        assert_eq!(preview.kind, LinkPreviewKind::ClinicalTrialsGov);
        assert_eq!(preview.title, "A Trial");
        assert_eq!(
            preview.details,
            [
                "Trial: NCT01234567",
                "Status: RECRUITING",
                "Type: INTERVENTIONAL",
                "Condition: Condition A"
            ]
        );
    }

    #[test]
    fn maps_osf_json_to_a_registration_card() {
        let registration = json!({"data":{"attributes":{"title":"Registered study", "date_registered":"2026-07-19T12:00:00Z", "provider":"OSF"}}});
        let preview = parse_osf_registration(&registration, "ab12c").unwrap();
        assert_eq!(preview.kind, LinkPreviewKind::Osf);
        assert_eq!(preview.title, "Registered study");
        assert_eq!(
            preview.details,
            [
                "Registration: ab12c",
                "Registered: 2026-07-19T12:00:00Z",
                "Provider: OSF"
            ]
        );
    }

    #[test]
    fn card_details_are_bounded() {
        assert_eq!(
            normalize_details(vec![
                "A".into(),
                "B".into(),
                "C".into(),
                "D".into(),
                "E".into()
            ])
            .len(),
            MAX_DETAIL_LINES
        );
    }
}
