#![cfg(test)]

use ferrodb_api::{RiskKind, Severity};

use super::*;

fn labels(items: &[CompletionItem]) -> Vec<&str> {
    items.iter().map(|i| i.label.as_str()).collect()
}

#[test]
fn catalog_is_consistent() {
    assert!(commands().len() > 300);
    for c in commands() {
        assert_ne!(c.arity, 0, "{}", c.name);
        if let Some((container, _)) = c.name.split_once(' ') {
            assert!(command(container).is_some(), "{} has no container", c.name);
        }
    }
    let set = command("set").unwrap();
    assert_eq!(set.arity, -3);
    assert!(set.has_flag("write"));
    assert!(command("CONFIG GET").is_some());
    assert!(command("json.set").is_some());
    assert!(command("FT.SEARCH").is_some());
}

#[test]
fn arguments_split_like_redis_cli() {
    let src =
        "SET \"a key\" 'it\\'s' \"x\\ny\\x41\"\n# comment\n  GET k\n\nHSET h f \"multi\nline\"";
    let lines = parse(src);
    assert_eq!(lines.len(), 3);
    let v: Vec<&str> = lines[0].args.iter().map(|a| a.value.as_str()).collect();
    assert_eq!(v, vec!["SET", "a key", "it's", "x\nyA"]);
    assert_eq!(&src[lines[1].range.clone()], "GET k");
    assert_eq!(lines[2].args[3].value, "multi\nline");
    assert_eq!(split(src).len(), 3);
}

#[test]
fn classification() {
    assert!(is_write("set a 1"));
    assert!(is_write("DEL a b"));
    assert!(is_write("FLUSHDB"));
    assert!(!is_write("GET a"));
    assert!(!is_write("SCAN 0 MATCH x*"));
    assert!(!is_write("CONFIG GET maxmemory"));
    assert!(is_write("frobnicate"));
    assert!(is_dangerous("FLUSHALL"));
    assert!(is_dangerous("config set maxmemory 1mb"));
    assert!(is_dangerous("KEYS *"));
    assert!(!is_dangerous("GET a"));
    assert!(is_blocking("BLPOP q 0"));
    assert!(is_blocking("XREAD BLOCK 0 STREAMS s $"));
    assert!(!is_blocking("XREAD COUNT 1 STREAMS s 0"));
    assert!(is_blocking("SUBSCRIBE news"));
    assert!(is_subscribe("psubscribe n*"));
    assert!(!is_blocking("LPOP q"));
}

#[test]
fn risks_need_confirmation() {
    let r = risks("GET a\nFLUSHDB\nSET a 1\nSHUTDOWN", false);
    let kinds: Vec<RiskKind> = r.iter().map(|r| r.kind).collect();
    assert_eq!(kinds, vec![RiskKind::Truncate, RiskKind::Modification]);
    let all = risks("GET a\nSET a 1", true);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].range, 6..13);
}

#[test]
fn diagnostics_arity_and_unknown() {
    let d = diagnostics("GET\nSETT a 1\nGET a\nCONFIG\nCONFIG FOO\nKEYS *\nSET a 1 \"x");
    let msgs: Vec<&str> = d.iter().map(|d| d.message.as_str()).collect();
    assert!(
        msgs[0].starts_with("Wrong number of arguments for GET"),
        "{msgs:?}"
    );
    assert!(msgs[1].contains("did you mean SET"), "{msgs:?}");
    assert!(msgs[2].contains("needs a subcommand"), "{msgs:?}");
    assert!(msgs[3].contains("Unknown subcommand 'FOO'"), "{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("Unterminated")), "{msgs:?}");
    assert!(
        d.iter()
            .any(|d| d.severity == Severity::Warning && d.message.contains("SCAN"))
    );
    assert!(diagnostics("SET a 1 EX 10\nmget a b c\nCONFIG GET *").is_empty());
}

#[test]
fn completion_of_commands_subcommands_options_and_keys() {
    let c = complete("hge", 3, &[]);
    assert_eq!(c[0].label, "HGET");
    assert!(labels(&c).contains(&"HGETALL"));
    assert_eq!(c[0].replace, 0..3);
    let c = complete("CONFIG ", 7, &[]);
    assert!(labels(&c).contains(&"GET") && labels(&c).contains(&"SET"));
    let keys = vec![
        "user:1:name".to_owned(),
        "user:2:name".to_owned(),
        "session:x".to_owned(),
        "a key".to_owned(),
    ];
    let c = complete("GET us", 6, &keys);
    assert!(labels(&c).contains(&"user:1:name"), "{:?}", labels(&c));
    assert!(labels(&c).contains(&"user:"));
    let c = complete("GET ", 4, &keys);
    let k = c.iter().find(|i| i.label == "a key").unwrap();
    assert_eq!(k.insert_text, "\"a key\"");
    // Options of SET after the value; no keys there.
    let c = complete("SET k v ", 8, &keys);
    assert!(labels(&c).contains(&"NX") && labels(&c).contains(&"KEEPTTL"));
    assert!(!labels(&c).contains(&"user:1:name"));
    // MSET keys at odd positions.
    let c = complete("MSET a 1 ", 9, &keys);
    assert!(labels(&c).contains(&"session:x"));
    // Second line.
    let c = complete("GET a\nzra", 9, &[]);
    assert!(labels(&c).contains(&"ZRANGE"));
}

#[test]
fn signature_help_and_hover() {
    let h = signature_help("SET k ", 6).unwrap();
    let s = &h.signatures[0];
    assert!(s.label.starts_with("SET key value"));
    assert_eq!(h.active_parameter, 1);
    assert_eq!(&s.label[s.parameters[0].clone()], "key");
    assert_eq!(&s.label[s.parameters[1].clone()], "value");
    let h = hover("hgetall h", 2).unwrap();
    assert!(h.contents.contains("HGETALL") && h.contents.contains("hash"));
    assert_eq!(h.range, 0..7);
    assert!(
        hover("config get maxmemory", 8)
            .unwrap()
            .contents
            .contains("CONFIG GET")
    );
}

#[test]
fn format_is_idempotent_and_keeps_arguments() {
    let src = "# keys\nset   a   'x  y'\n  config   get *\nget a";
    let f = format(src);
    assert_eq!(f, "# keys\nSET a 'x  y'\nCONFIG GET *\nGET a");
    assert_eq!(format(&f), f);
}
