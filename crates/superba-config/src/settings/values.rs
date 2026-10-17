//! Current values of `config.toml`, flattened to dotted paths.

use std::collections::BTreeMap;

use superba_core::{Error, Result, SettingValue, is_valid_setting_path};

/// Every value in `config.toml` by dotted path (`features.fast_mode`,
/// `mcp_servers.docs.command`). Tables, standard or inline, are descended into;
/// arrays (also arrays of tables) are leaves. Keys that are not bare TOML keys
/// cannot be addressed by Superba and are left out, as are date-times. `None` or
/// empty input gives an empty map; invalid TOML is [`Error::Parse`].
pub fn read_config_values(config_toml: Option<&[u8]>) -> Result<BTreeMap<String, SettingValue>> {
    let mut out = BTreeMap::new();
    let Some(bytes) = config_toml else {
        return Ok(out);
    };
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text =
        std::str::from_utf8(bytes).map_err(|e| Error::parse("config.toml", e.to_string()))?;
    let table: toml::Table =
        toml::from_str(text).map_err(|e| Error::parse("config.toml", e.to_string()))?;
    flatten("", &table, &mut out);
    Ok(out)
}

fn flatten(prefix: &str, table: &toml::Table, out: &mut BTreeMap<String, SettingValue>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if !is_valid_setting_path(&path) {
            continue;
        }
        match value {
            toml::Value::Table(t) => flatten(&path, t, out),
            other => {
                if let Some(v) = SettingValue::from_toml(other) {
                    out.insert(path, v);
                }
            }
        }
    }
}
