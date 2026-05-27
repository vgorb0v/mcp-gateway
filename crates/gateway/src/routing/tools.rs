pub fn split_prefixed_tool_name(name: &str) -> Option<(&str, &str)> {
    name.split_once('.')
}
