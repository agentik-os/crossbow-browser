//! Each Worker's wrangler.example.toml is what a public snapshot ships in
//! place of its real wrangler.toml, which names this account's Workers and D1
//! databases. Each pair must not drift: the example has every table, binding,
//! var and migration of the real file, and the same values except where it
//! holds a `<your-...>` placeholder.

use super::rules::{Table, Value, parse_toml};

/// Each Worker's real file, then its example.
const PAIRS: [(&str, &str); 2] = [
    ("cloud/api/wrangler.toml", "cloud/api/wrangler.example.toml"),
    ("cloud/ai/wrangler.toml", "cloud/ai/wrangler.example.toml"),
];

/// Each table as `name#n` (its n-th appearance), so array tables pair up in
/// order. The keys before the first table go in `[top-level]`.
fn tables(text: &str, what: &str) -> Vec<(String, Table)> {
    let parsed = parse_toml(&format!("[top-level]\n{text}")).unwrap_or_else(|e| panic!("{what}, read after an added `[top-level]` line: {e}"));
    let mut seen: Vec<String> = Vec::new();
    parsed
        .into_iter()
        .map(|table| {
            let n = seen.iter().filter(|name| **name == table.name).count();
            seen.push(table.name.clone());
            (format!("{}#{n}", table.name), table)
        })
        .collect()
}

fn is_placeholder(value: &Value) -> bool {
    let Value::Str(text) = value else { return false };
    let Some(inner) = text.strip_prefix("<your-").and_then(|rest| rest.strip_suffix('>')) else { return false };
    !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// What differs between the two files, one line each; empty when they agree.
fn drift(real: &str, example: &str) -> Vec<String> {
    let real = tables(real, "the real file");
    let example = tables(example, "the example");
    let mut out = Vec::new();
    for (id, table) in &real {
        let Some((_, twin)) = example.iter().find(|(other, _)| other == id) else {
            out.push(format!("[{}] is missing", table.name));
            continue;
        };
        for (key, value, _) in &table.keys {
            match twin.keys.iter().find(|(other, _, _)| other == key) {
                None => out.push(format!("[{}] {key} is missing", table.name)),
                Some((_, mine, _)) if mine != value && !is_placeholder(mine) => {
                    out.push(format!("[{}] {key} differs and is not a <your-...> placeholder", table.name))
                }
                Some(_) => {}
            }
        }
        for (key, _, _) in &twin.keys {
            if !table.keys.iter().any(|(other, _, _)| other == key) {
                out.push(format!("[{}] {key} is only in the example", table.name));
            }
        }
    }
    for (id, table) in &example {
        if !real.iter().any(|(other, _)| other == id) {
            out.push(format!("[{}] is only in the example", table.name));
        }
    }
    out
}

#[test]
fn each_wrangler_example_has_every_binding_var_and_migration_of_its_real_file() {
    let root = crate::root();
    for (real, example) in PAIRS {
        let example_text = std::fs::read_to_string(root.join(example)).unwrap_or_else(|e| panic!("{example} ships in every snapshot: {e}"));
        // A public snapshot has only the example; parse it all the same.
        let Ok(real_text) = std::fs::read_to_string(root.join(real)) else {
            tables(&example_text, example);
            continue;
        };
        let found = drift(&real_text, &example_text);
        assert!(found.is_empty(), "{example} has drifted from {real}; change the example to match:\n{}", found.join("\n"));
    }
}

#[test]
fn drift_names_each_missing_or_changed_name() {
    let real = "name = \"x\"\n[vars]\nENVIRONMENT = \"dev\"\nMAIL_FROM = \"a\"\n[[d1_databases]]\nbinding = \"DB\"\ndatabase_id = \"1234\"\n[[migrations]]\ntag = \"v1\"\n";
    let good = "name = \"<your-worker-name>\"\n[vars]\nENVIRONMENT = \"dev\"\nMAIL_FROM = \"<your-mail-from>\"\n[[d1_databases]]\nbinding = \"DB\"\ndatabase_id = \"<your-d1-database-id>\"\n[[migrations]]\ntag = \"v1\"\n";
    assert_eq!(drift(real, good), Vec::<String>::new());
    let bad = "name = \"<your-worker-name>\"\n[vars]\nENVIRONMENT = \"prod\"\nEXTRA = \"1\"\n[[d1_databases]]\nbinding = \"DATABASE\"\ndatabase_id = \"<your d1 id>\"\n[[d1_databases]]\nbinding = \"DB\"\n";
    assert_eq!(
        drift(real, bad),
        [
            "[vars] ENVIRONMENT differs and is not a <your-...> placeholder",
            "[vars] MAIL_FROM is missing",
            "[vars] EXTRA is only in the example",
            "[d1_databases] binding differs and is not a <your-...> placeholder",
            "[d1_databases] database_id differs and is not a <your-...> placeholder",
            "[migrations] is missing",
            "[d1_databases] is only in the example",
        ]
    );
}
