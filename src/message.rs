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
