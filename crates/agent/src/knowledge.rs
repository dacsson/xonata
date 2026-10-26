//! Local specifications exposed only through bounded, on-demand tool reads.
use serde_json::{Value, json};

struct Specification {
    id: &'static str,
    title: &'static str,
    text: &'static str,
}

const SPECIFICATIONS: &[Specification] = &[Specification {
    id: "kanata-trace",
    title: "Kanata trace",
    text: include_str!("../knowledge/kanata-trace.md"),
}];

pub(crate) fn description() -> String {
    let titles = SPECIFICATIONS
        .iter()
        .map(|spec| format!("{} ({})", spec.title, spec.id))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Read a local specification only when needed for format semantics. Available: {titles}. Supply id; start is a decimal byte offset, default 0. Each page contains at most 2048 UTF-8 bytes; use data.next_start to continue. Specifications describe formats, not observed trace behavior."
    )
}

pub(crate) fn read(id: &str, start: u64) -> Result<Value, String> {
    let spec = SPECIFICATIONS
        .iter()
        .find(|spec| spec.id == id)
        .ok_or("Unknown specification ID")?;
    let (text, next) = page(spec.text, start)?;
    Ok(json!({"id":spec.id, "title":spec.title, "text":text,
        "next_start":next.map(|n| n.to_string()), "total_bytes":spec.text.len().to_string()}))
}

fn page(text: &str, start: u64) -> Result<(&str, Option<usize>), String> {
    let start = usize::try_from(start).map_err(|_| "Specification offset is out of range")?;
    if !text.is_char_boundary(start) {
        return Err("Use a valid UTF-8 byte offset from next_start".into());
    }
    let mut end = start.saturating_add(2048).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok((&text[start..end], (end < text.len()).then_some(end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specification_pages_preserve_utf8_and_reject_invalid_offsets() {
        let text = "界".repeat(1000);
        let (first, next) = page(&text, 0).unwrap();
        assert_eq!(first.len(), 2046);
        let (last, next) = page(&text, next.unwrap() as u64).unwrap();
        assert_eq!(format!("{first}{last}"), text);
        assert_eq!(next, None);
        assert!(page(&text, 1).is_err());
        assert!(page(&text, u64::MAX).is_err());
        assert!(read("unknown", 0).is_err());
        assert!(!description().contains("Kanata\\t0004"));
    }
}
