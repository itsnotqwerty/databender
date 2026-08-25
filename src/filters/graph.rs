use crate::{DatabenderError, Result};

const MAX_GRAPH_BYTES: usize = 4096;
const RESOURCE_FILTERS: &[&str] = &[
    "amovie",
    "asendcmd",
    "ass",
    "azmq",
    "drawtext",
    "frei0r",
    "ladspa",
    "lut3d",
    "lv2",
    "movie",
    "sendcmd",
    "subtitles",
    "zmq",
];
const RESOURCE_OPTIONS: &[&str] = &[
    "commands", "filename", "file", "fontfile", "textfile", "url",
];
const PROTOCOLS: &[&str] = &[
    "concat:", "crypto:", "data:", "file:", "ftp:", "http:", "https:", "pipe:", "rtmp:", "rtsp:",
    "sftp:", "subfile:", "tcp:", "udp:",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpertGraph {
    fragment: String,
    filters: Vec<String>,
}

impl ExpertGraph {
    pub fn parse(fragment: &str) -> Result<Self> {
        if fragment.is_empty() || fragment.len() > MAX_GRAPH_BYTES {
            return Err(invalid_graph(format!(
                "graph length must be between 1 and {MAX_GRAPH_BYTES} bytes"
            )));
        }
        if fragment.chars().any(|character| {
            character.is_control() || matches!(character, '$' | '`' | ';' | '[' | ']')
        }) {
            return Err(invalid_graph(
                "control characters, shell interpolation, labels, and multiple chains are not permitted",
            ));
        }
        let lowercase = fragment.to_ascii_lowercase();
        if PROTOCOLS
            .iter()
            .any(|protocol| lowercase.contains(protocol))
        {
            return Err(invalid_graph("external protocols are not permitted"));
        }

        let mut filters = Vec::new();
        for node in split_nodes(fragment)? {
            let node = node.trim();
            let name_end = node
                .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .unwrap_or(node.len());
            let name = &node[..name_end];
            if name.is_empty() || !matches!(node.as_bytes().get(name_end), None | Some(b'=')) {
                return Err(invalid_graph(format!("invalid filter node {node:?}")));
            }
            if RESOURCE_FILTERS.contains(&name) {
                return Err(invalid_graph(format!(
                    "filter {name} can access external resources"
                )));
            }
            if let Some(arguments) = node.get(name_end + 1..) {
                for argument in arguments.split(':') {
                    let key = argument
                        .split_once('=')
                        .map(|(key, _)| key.trim())
                        .unwrap_or_default();
                    if RESOURCE_OPTIONS.contains(&key) {
                        return Err(invalid_graph(format!(
                            "option {key} can access external resources"
                        )));
                    }
                }
            }
            filters.push(name.to_owned());
        }
        Ok(Self {
            fragment: fragment.to_owned(),
            filters,
        })
    }

    pub fn fragment(&self) -> &str {
        &self.fragment
    }

    pub fn filters(&self) -> &[String] {
        &self.filters
    }
}

fn split_nodes(fragment: &str) -> Result<Vec<&str>> {
    let mut nodes = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in fragment.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '\'' => quoted = !quoted,
            ',' if !quoted => {
                if index == start {
                    return Err(invalid_graph("graph contains an empty filter node"));
                }
                nodes.push(&fragment[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if escaped || quoted {
        return Err(invalid_graph(
            "graph contains an unterminated escape or quote",
        ));
    }
    if start == fragment.len() {
        return Err(invalid_graph("graph contains an empty filter node"));
    }
    nodes.push(&fragment[start..]);
    Ok(nodes)
}

fn invalid_graph(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::InvalidParameter {
        parameter: "expert graph".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_single_chain_graphs() {
        let graph = ExpertGraph::parse("hue=h=30,eq=contrast=1.2").unwrap();

        assert_eq!(graph.fragment(), "hue=h=30,eq=contrast=1.2");
        assert_eq!(graph.filters(), ["hue", "eq"]);
    }

    #[test]
    fn supports_quoted_or_escaped_argument_commas() {
        assert_eq!(
            ExpertGraph::parse("select='eq(n,1)',hue=h=20")
                .unwrap()
                .filters(),
            ["select", "hue"]
        );
        assert_eq!(
            ExpertGraph::parse("select=eq(n\\,1),hue=h=20")
                .unwrap()
                .filters(),
            ["select", "hue"]
        );
    }

    #[test]
    fn rejects_interpolation_protocols_and_external_resources() {
        for graph in [
            "movie=file:/tmp/input.mp4",
            "subtitles=/tmp/captions.srt",
            "drawtext=textfile=caption.txt",
            "volume=$GAIN",
            "hue=h=20;eq=contrast=2",
            "[in]hue[out]",
        ] {
            assert!(ExpertGraph::parse(graph).is_err(), "accepted {graph}");
        }
    }
}
