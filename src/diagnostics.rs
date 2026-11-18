//! Diagnostics retain error location and category without including input data.

pub(crate) fn json(error: &serde_json::Error) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "I/O failure",
        serde_json::error::Category::Syntax => "invalid syntax",
        serde_json::error::Category::Data => "invalid value or type",
        serde_json::error::Category::Eof => "incomplete input",
    };
    format!(
        "JSON {category} at line {}, column {}",
        error.line(),
        error.column()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_errors_keep_location_without_echoing_values() {
        let error = serde_json::from_str::<crate::settings::AppSettings>(
            "{\n  \"preferences\": {\"theme\": \"SYNTHETIC_PRIVATE_VALUE\"}\n}",
        )
        .unwrap_err();
        // Demonstrate why Error::Display cannot be used on untrusted data.
        assert!(error.to_string().contains("SYNTHETIC_PRIVATE_VALUE"));
        assert_eq!(
            json(&error),
            format!(
                "JSON invalid value or type at line 2, column {}",
                error.column()
            )
        );
        for (input, expected) in [("{", "incomplete input"), ("{!}", "invalid syntax")] {
            let error = serde_json::from_str::<serde_json::Value>(input).unwrap_err();
            assert!(json(&error).contains(expected));
        }
    }
}
