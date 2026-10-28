//! Tools a front end provides: the desktop app's browser, for one. The agent
//! lists and calls them like any other tool; a call becomes a `tool/call`
//! request to the front end over the app server's JSON-RPC, and the answer is
//! the tool's result.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

/// How long a front end has to answer one call. A page load or a click that
/// navigates can take a while; a front end that went away should not hang the
/// turn forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// What a front end answered: text for the model, and images it should see.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ClientResult {
    pub text: String,
    pub images: Vec<String>,
}

/// One call on its way to the front end.
pub struct ClientCall {
    pub name: String,
    pub arguments: Value,
    pub respond: oneshot::Sender<ClientResult>,
}

#[derive(Debug, Clone)]
struct ClientTool {
    description: String,
    parameters: Value,
    needs_approval: bool,
}

/// The tools a front end registered, and the line to it.
#[derive(Debug)]
pub struct ClientTools {
    tools: HashMap<String, ClientTool>,
    calls: mpsc::UnboundedSender<ClientCall>,
}

impl ClientTools {
    /// Read `tools/register` params: `{tools: [{name, description, parameters,
    /// needsApproval}]}`. A name the agent already has is refused rather than
    /// shadowing it.
    pub fn from_params(params: &Value, taken: &[String], calls: mpsc::UnboundedSender<ClientCall>) -> Result<Self> {
        let mut tools = HashMap::new();
        for tool in params["tools"].as_array().ok_or_else(|| anyhow!("tools must be an array"))? {
            let name = tool["name"].as_str().ok_or_else(|| anyhow!("a tool needs a name"))?;
            if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
                bail!("`{name}` is not a valid tool name");
            }
            if taken.iter().any(|existing| existing == name) {
                bail!("`{name}` is already a tool");
            }
            tools.insert(
                name.to_owned(),
                ClientTool {
                    description: tool["description"].as_str().unwrap_or_default().to_owned(),
                    parameters: tool
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                    needs_approval: tool["needsApproval"].as_bool().unwrap_or(true),
                },
            );
        }
        Ok(Self { tools, calls })
    }

    pub fn tool_specs(&self) -> Vec<Value> {
        let mut names: Vec<&String> = self.tools.keys().collect();
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let tool = &self.tools[name];
                json!({
                    "type": "function",
                    "function": {"name": name, "description": tool.description, "parameters": tool.parameters},
                })
            })
            .collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.tools.keys()
    }

    pub fn needs_approval(&self, name: &str) -> Option<bool> {
        self.tools.get(name).map(|tool| tool.needs_approval)
    }

    /// Ask the front end to run `name`; None when it is not one of these tools.
    pub async fn execute(&self, name: &str, arguments: &str) -> Option<ClientResult> {
        if !self.tools.contains_key(name) {
            return None;
        }
        let arguments = match crate::tools::call_arguments(arguments) {
            Ok(value) => value,
            Err(error) => return Some(error_result(&format!("invalid arguments: {error}"))),
        };
        let (respond, answer) = oneshot::channel();
        if self.calls.send(ClientCall { name: name.to_owned(), arguments, respond }).is_err() {
            return Some(error_result("the front end that provides this tool is gone"));
        }
        Some(match tokio::time::timeout(CALL_TIMEOUT, answer).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => error_result("the front end dropped the call"),
            Err(_) => error_result(&format!("no answer from the front end in {}s", CALL_TIMEOUT.as_secs())),
        })
    }
}

fn error_result(message: &str) -> ClientResult {
    ClientResult { text: format!("Error: {message}"), images: Vec::new() }
}

/// Read a front end's answer to `tool/call`: `{content, images?}` on success,
/// or a JSON-RPC error.
pub fn read_result(result: &Value, error: Option<&Value>) -> ClientResult {
    if let Some(error) = error {
        return error_result(error["message"].as_str().unwrap_or("the tool failed"));
    }
    ClientResult {
        text: result["content"].as_str().unwrap_or_default().to_owned(),
        images: result["images"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|url| crate::context::check_image_url(url).is_ok())
            .map(str::to_owned)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser(taken: &[String]) -> Result<(ClientTools, mpsc::UnboundedReceiver<ClientCall>)> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let params = json!({"tools": [
            {"name": "browser_navigate", "description": "Open a URL", "needsApproval": false,
             "parameters": {"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}},
            {"name": "browser_click", "description": "Click an element"},
        ]});
        Ok((ClientTools::from_params(&params, taken, sender)?, receiver))
    }

    #[test]
    fn registered_tools_are_listed_with_their_approval_rule() {
        let (tools, _) = browser(&[]).unwrap();
        let specs = tools.tool_specs();
        let names: Vec<&str> = specs.iter().map(|spec| spec["function"]["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["browser_click", "browser_navigate"]);
        assert_eq!(tools.needs_approval("browser_navigate"), Some(false));
        assert_eq!(tools.needs_approval("browser_click"), Some(true), "asks unless told otherwise");
        assert_eq!(tools.needs_approval("read_file"), None);
        assert!(browser(&["browser_click".into()]).is_err(), "a front end cannot shadow a tool");
    }

    #[tokio::test]
    async fn a_call_goes_to_the_front_end_and_its_answer_comes_back() {
        let (tools, mut calls) = browser(&[]).unwrap();
        let front_end = tokio::spawn(async move {
            let call = calls.recv().await.unwrap();
            assert_eq!(call.name, "browser_navigate");
            assert_eq!(call.arguments["url"], "https://example.com");
            let answer = read_result(
                &json!({"content": "Loaded Example Domain", "images": ["data:image/png;base64,QUJD", "nope"]}),
                None,
            );
            call.respond.send(answer).unwrap();
        });
        let result = tools.execute("browser_navigate", r#"{"url":"https://example.com"}"#).await.unwrap();
        front_end.await.unwrap();
        assert_eq!(result.text, "Loaded Example Domain");
        assert_eq!(result.images, ["data:image/png;base64,QUJD"], "only real images pass");
        assert!(tools.execute("read_file", "{}").await.is_none());
    }

    #[tokio::test]
    async fn a_front_end_error_or_departure_is_an_error_result() {
        let (tools, calls) = browser(&[]).unwrap();
        assert!(read_result(&Value::Null, Some(&json!({"message": "no tab open"}))).text.contains("no tab open"));
        drop(calls);
        let result = tools.execute("browser_click", r#"{"ref": 3}"#).await.unwrap();
        assert!(result.text.starts_with("Error:"), "{}", result.text);
    }

    /// A model may call a tool that takes nothing with no arguments at all;
    /// that once failed every `browser_screenshot` before it reached the window.
    #[tokio::test]
    async fn a_call_with_no_arguments_reaches_the_front_end() {
        let (tools, mut calls) = browser(&[]).unwrap();
        let front_end = tokio::spawn(async move {
            let call = calls.recv().await.unwrap();
            assert_eq!(call.arguments, json!({}));
            call.respond.send(ClientResult { text: "Clicked".into(), images: Vec::new() }).unwrap();
        });
        let result = tools.execute("browser_click", "").await.unwrap();
        front_end.await.unwrap();
        assert_eq!(result.text, "Clicked");
    }
}
