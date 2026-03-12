use std::collections::HashMap;

#[derive(Debug)]
pub struct Message {
    pub code: u32,
    #[allow(dead_code)]
    pub description: String,
    pub fields: HashMap<String, String>,
}

impl Message {
    pub fn parse(input: &str) -> Option<Self> {
        let mut lines = input.lines();
        let header = lines.next()?;
        let (code_str, description) = header.split_once(' ')?;
        let code = code_str.parse().ok()?;

        let mut fields = HashMap::new();
        for line in lines {
            if let Some((key, value)) = line.split_once(": ") {
                let entry: &mut String = fields.entry(key.trim().to_string()).or_default();
                if !entry.is_empty() {
                    entry.push('\n');
                }
                entry.push_str(value.trim());
            }
        }

        Some(Message {
            code,
            description: description.to_string(),
            fields,
        })
    }

    pub fn format(code: u32, description: &str, fields: &[(&str, &str)]) -> String {
        let mut out = format!("{} {}\n", code, description);
        for (key, value) in fields {
            out.push_str(&format!("{}: {}\n", key, value));
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Message::parse ---

    #[test]
    fn test_parse_simple_message() {
        let input =
            "600 URI Acquire\nURI: r2://apt-private/pool/main/pkg.deb\nFilename: /tmp/pkg.deb\n";
        let msg = Message::parse(input).unwrap();
        assert_eq!(msg.code, 600);
        assert_eq!(msg.description, "URI Acquire");
        assert_eq!(msg.fields["URI"], "r2://apt-private/pool/main/pkg.deb");
        assert_eq!(msg.fields["Filename"], "/tmp/pkg.deb");
    }

    #[test]
    fn test_parse_multiple_config_items() {
        let input = "601 Configuration\n\
            Config-Item: Acquire::r2::AccessKeyId=mykey\n\
            Config-Item: Acquire::r2::SecretAccessKey=mysecret\n\
            Config-Item: Acquire::r2::Bucket=apt-private\n";
        let msg = Message::parse(input).unwrap();
        assert_eq!(msg.code, 601);
        let config = &msg.fields["Config-Item"];
        assert!(config.contains("Acquire::r2::AccessKeyId=mykey"));
        assert!(config.contains("Acquire::r2::SecretAccessKey=mysecret"));
        assert!(config.contains("Acquire::r2::Bucket=apt-private"));
    }

    #[test]
    fn test_parse_returns_none_on_empty() {
        assert!(Message::parse("").is_none());
    }

    // --- Message::format ---

    #[test]
    fn test_parse_returns_none_on_missing_code() {
        assert!(Message::parse("not a valid header\nFoo: bar\n").is_none());
    }

    #[test]
    fn test_format_capabilities() {
        let output = Message::format(
            100,
            "Capabilities",
            &[("Version", "1.0"), ("Single-Instance", "true")],
        );
        assert!(output.starts_with("100 Capabilities\n"));
        assert!(output.contains("Version: 1.0\n"));
        assert!(output.contains("Single-Instance: true\n"));
        assert!(output.ends_with("\n\n"));
    }

    #[test]
    fn test_format_empty_fields() {
        let output = Message::format(401, "General Failure", &[]);
        assert_eq!(output, "401 General Failure\n\n");
    }
}
