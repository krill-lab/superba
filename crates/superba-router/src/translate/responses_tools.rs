//! Responses tool definitions, including namespaces and text-input tools.
//! A stable JSON-function identity crosses the IR; this codec restores the
//! native tool shape using the request's definitions on the response path.

use super::super::ir::{NativeTool, Tool};
use super::super::{Invalid, array, kind, optional_string, string};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn name(name: &str, namespace: Option<&str>, text: bool) -> String {
    if namespace.is_none() && !text {
        return name.to_owned();
    }
    let identity = json!([namespace, name, text]);
    let hash = Sha256::digest(identity.to_string().as_bytes());
    format!("spb_{}", hex::encode(&hash[..20]))
}

pub(super) fn decode(
    values: &[Value],
    namespace: Option<&str>,
    out: &mut Vec<Tool>,
) -> Result<(), Invalid> {
    for value in values {
        if kind(value) == "namespace" {
            if namespace.is_some() {
                return Err(Invalid::new("nested tool namespaces are unsupported"));
            }
            decode(array(value, "tools"), value["name"].as_str(), out)?;
            continue;
        }
        let text = match kind(value) {
            "function" => false,
            "custom" => true,
            _ => {
                return Err(Invalid::new(
                    "only callable tools can be translated from Responses",
                ));
            }
        };
        let original = string(value, "name");
        if original.is_empty() {
            return Err(Invalid::new("tool definition needs a name"));
        }
        let format = text.then(|| {
            value
                .get("format")
                .cloned()
                .unwrap_or_else(|| json!({"type":"text"}))
        });
        let parameters = if text {
            let description = match format
                .as_ref()
                .and_then(|f| f.get("definition"))
                .and_then(Value::as_str)
            {
                Some(grammar) => {
                    format!("Raw text input. It must conform to this grammar:\n{grammar}")
                }
                None => "Raw text input accepted by the tool.".into(),
            };
            json!({"type":"object","properties":{"input":{"type":"string","description":description}},"required":["input"],"additionalProperties":false})
        } else {
            value
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}))
        };
        out.push(Tool {
            name: name(&original, namespace, text),
            description: optional_string(value, "description"),
            parameters,
            strict: value["strict"].as_bool(),
            native: (namespace.is_some() || text).then(|| NativeTool {
                name: original,
                namespace: namespace.map(str::to_owned),
                format,
            }),
        });
    }
    Ok(())
}

pub(super) fn encode(tools: &[Tool]) -> Vec<Value> {
    let mut out = Vec::new();
    for tool in tools {
        let native = tool.native.as_ref();
        let text = native.and_then(|n| n.format.as_ref());
        let mut value = json!({"type":if text.is_some() {"custom"} else {"function"},"name":native.map(|n| &n.name).unwrap_or(&tool.name)});
        if let Some(format) = text {
            value["format"] = format.clone();
        } else {
            value["parameters"] = tool.parameters.clone();
            if let Some(strict) = tool.strict {
                value["strict"] = strict.into();
            }
        }
        if let Some(description) = &tool.description {
            value["description"] = description.clone().into();
        }
        if let Some(namespace) = native.and_then(|n| n.namespace.as_ref()) {
            if let Some(group) = out
                .iter_mut()
                .find(|v: &&mut Value| v["type"] == "namespace" && v["name"] == *namespace)
            {
                group["tools"]
                    .as_array_mut()
                    .expect("namespace tools")
                    .push(value);
            } else {
                out.push(json!({"type":"namespace","name":namespace,"tools":[value]}));
            }
        } else {
            out.push(value);
        }
    }
    out
}

#[derive(Default)]
pub(crate) struct Bindings {
    tools: BTreeMap<String, NativeTool>,
    text_items: BTreeSet<String>,
    text_calls: BTreeSet<String>,
}

impl Bindings {
    pub(crate) fn new(tools: &[Tool]) -> Self {
        Self {
            tools: tools
                .iter()
                .filter_map(|t| t.native.clone().map(|n| (t.name.clone(), n)))
                .collect(),
            ..Self::default()
        }
    }

    fn input(arguments: &str) -> Result<String, Invalid> {
        if arguments.is_empty() {
            return Ok(String::new());
        }
        let value: Value = serde_json::from_str(arguments)
            .map_err(|_| Invalid::new("text tool arguments are not JSON"))?;
        value["input"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Invalid::new("text tool requires a string input"))
    }

    pub(crate) fn item(&mut self, item: &mut Value) -> Result<(), Invalid> {
        if kind(item) == "function_call_output"
            && self.text_calls.contains(&string(item, "call_id"))
        {
            item["type"] = "custom_tool_call_output".into();
        }
        if kind(item) != "function_call" {
            return Ok(());
        }
        let Some(native) = self.tools.get(&string(item, "name")) else {
            return Ok(());
        };
        item["name"] = native.name.clone().into();
        if let Some(namespace) = &native.namespace {
            item["namespace"] = namespace.clone().into();
        }
        if native.format.is_some() {
            let id = string(item, "id");
            self.text_items.insert(id.clone());
            self.text_calls.insert(string(item, "call_id"));
            item["type"] = "custom_tool_call".into();
            item["input"] = Self::input(&string(item, "arguments"))?.into();
            item.as_object_mut()
                .expect("tool object")
                .remove("arguments");
            if !id.is_empty() {
                item["id"] = id.replacen("fc_", "ctc_", 1).into();
            }
        }
        Ok(())
    }

    pub(crate) fn response(&mut self, value: &mut Value) -> Result<(), Invalid> {
        if let Some(items) = value["output"].as_array_mut() {
            for item in items {
                self.item(item)?;
            }
        }
        Ok(())
    }

    pub(crate) fn events(&mut self, values: Vec<Value>) -> Result<Vec<Value>, Invalid> {
        let mut out = Vec::new();
        for mut value in values {
            if self.text_items.contains(&string(&value, "item_id")) {
                match kind(&value) {
                    "response.function_call_arguments.delta" => continue,
                    "response.function_call_arguments.done" => {
                        let input = Self::input(&string(&value, "arguments"))?;
                        value["item_id"] =
                            string(&value, "item_id").replacen("fc_", "ctc_", 1).into();
                        let mut delta = value.clone();
                        delta["type"] = "response.custom_tool_call_input.delta".into();
                        delta["delta"] = input.clone().into();
                        delta
                            .as_object_mut()
                            .expect("event object")
                            .remove("arguments");
                        out.push(delta);
                        value["type"] = "response.custom_tool_call_input.done".into();
                        value["input"] = input.into();
                        value
                            .as_object_mut()
                            .expect("event object")
                            .remove("arguments");
                    }
                    _ => {}
                }
            }
            if let Some(item) = value.get_mut("item") {
                self.item(item)?;
            }
            if let Some(response) = value.get_mut("response") {
                self.response(response)?;
            }
            out.push(value);
        }
        Ok(out)
    }
}
