//! The agent catalog: what HarnessMesh knows about each agent it can find,
//! install and start, one row each. Detection, the one-click install, launch
//! arguments, the panel's onboarding dialog and the CLI read this table, so
//! another ACP agent is another row.
//!
//! What a row cannot say stays code keyed by the agent's [`Wire`] kind: how
//! its model-service settings are written, and the session-flow fixes for a
//! particular agent. A row added later is [`Wire::Acp`] and gets the generic
//! ACP behaviour there.
//!
//! Other computers see an agent only as one of the six [`Wire`] kinds every
//! HarnessMesh version accepts: older versions read agent cards strictly and
//! would reject an agent list holding any other value. A card therefore
//! carries [`HarnessKind::family`]; which catalog row a computer's agent
//! comes from travels separately, as [`AgentKindNote`]s.
use crate::service::Protocol;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    ffi::OsString,
    fmt,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::LazyLock,
};
use uuid::Uuid;

/// The kinds every HarnessMesh version accepts in an agent card. Older
/// versions parse them as a closed set, so this list is frozen: an agent
/// added to the catalog is `Acp` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    Pi,
    Dsh,
    Opencode,
    ClaudeCode,
    Codex,
    Acp,
}

/// One agent HarnessMesh knows by name.
#[derive(Debug, PartialEq, Eq)]
pub struct AgentSpec {
    /// Catalog key, as this computer's own configuration, detection and
    /// install name the agent (`"harness": "claude-code"`).
    pub kind: &'static str,
    /// What other computers see. A row's own kind for the six original
    /// agents; `Acp` for every row added after them.
    pub wire: Wire,
    /// Product name.
    pub label: &'static str,
    /// The command that speaks ACP, as its package installs it on PATH.
    pub command: Option<&'static str>,
    /// Arguments that start it in ACP mode when its configuration lists none.
    pub args: &'static [&'static str],
    /// The npm package that provides `command`.
    pub package: Option<&'static str>,
    /// The package's `bin` entry for `command`, relative to the package.
    pub entry: Option<&'static str>,
    /// A pinned version HarnessMesh installs by itself.
    pub install: Option<Install>,
    /// Where a Windows npm shim's real entry is, when that is not
    /// `node_modules/<package>/<entry>` next to it.
    pub shim: Option<Shim>,
    /// A read-only command of its own that tells whether it is logged in.
    pub login: Option<LoginCommand>,
    /// Protocols its model-service settings can use. Only rows with a
    /// [`Wire`] kind of their own have settings code, so only they list any.
    pub protocols: &'static [Protocol],
    /// `hmesh setup` names it `<node>-<setup_name>`; the kind when `None`.
    pub setup_name: Option<&'static str>,
    /// The `agentInfo.name` it reports in ACP `initialize`. Not used yet:
    /// kept for moving per-agent session fixes into the catalog later.
    pub reports_as: Option<&'static str>,
}

/// A version HarnessMesh installs into its own data directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Install {
    pub version: &'static str,
    /// Oldest Node.js the package declares it runs on.
    pub node: &'static str,
    /// A command the adapter starts, which must already be installed.
    pub needs: Option<Needed>,
    /// Rough size once installed, for packages large enough to mention.
    pub download_mb: Option<u32>,
    /// How long `npm install` may take, in seconds.
    pub timeout_s: u64,
    /// Place in install lists (`hmesh agent install`, the panel's offers and
    /// the scan of managed installs). Rows with the same place keep catalog
    /// order.
    pub order: u8,
    /// An environment variable naming the agent's own home directory, which
    /// must be usable before it is installed.
    pub home_variable: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Needed {
    pub command: &'static str,
    /// Oldest version the adapter supports.
    pub version: &'static str,
    /// What installs the command when it is missing.
    pub install: &'static str,
}

/// A Windows npm shim's real entry, relative to the shim's directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shim {
    At(&'static str),
    /// The package moved: the shim points at `legacy` when it names
    /// `legacy_marker`, or when the current entry is missing and the shim
    /// does not name `current_marker` either.
    Moved {
        legacy: &'static str,
        legacy_marker: &'static str,
        current_marker: &'static str,
    },
}

/// A status command, run with `args`. Its output is never kept beyond the
/// conclusion `reading` draws from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginCommand {
    /// `None`: the registered executable itself. Otherwise a command found
    /// next to it or on PATH.
    pub program: Option<&'static str>,
    pub args: &'static [&'static str],
    pub reading: LoginReading,
}

/// How a status command's output is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginReading {
    /// `opencode auth list`.
    OpencodeAuthList,
    /// `codex login status`.
    CodexLoginStatus,
    /// `pi --list-models`.
    PiListModels,
}

const ALL_PROTOCOLS: &[Protocol] = &[Protocol::OpenAi, Protocol::Anthropic, Protocol::Responses];

/// What every row starts from.
pub const BLANK: AgentSpec = AgentSpec {
    kind: "",
    wire: Wire::Acp,
    label: "",
    command: None,
    args: &[],
    package: None,
    entry: None,
    install: None,
    shim: None,
    login: None,
    protocols: &[],
    setup_name: None,
    reports_as: None,
};

pub const OPENCODE: AgentSpec = AgentSpec {
    kind: "opencode",
    wire: Wire::Opencode,
    label: "OpenCode",
    command: Some("opencode"),
    args: &["acp"],
    package: Some("opencode-ai"),
    shim: Some(Shim::At("node_modules/opencode-ai/bin/opencode.exe")),
    login: Some(LoginCommand {
        program: None,
        args: &["auth", "list"],
        reading: LoginReading::OpencodeAuthList,
    }),
    protocols: ALL_PROTOCOLS,
    reports_as: Some("OpenCode"),
    ..BLANK
};

pub const CLAUDE_CODE: AgentSpec = AgentSpec {
    kind: "claude-code",
    wire: Wire::ClaudeCode,
    label: "Claude Code",
    command: Some("claude-agent-acp"),
    package: Some("@agentclientprotocol/claude-agent-acp"),
    entry: Some("dist/index.js"),
    install: Some(Install {
        version: "0.109.0",
        node: "22.0",
        needs: None,
        download_mb: None,
        timeout_s: 120,
        order: 2,
        home_variable: None,
    }),
    protocols: &[Protocol::Anthropic],
    ..BLANK
};

pub const CODEX: AgentSpec = AgentSpec {
    kind: "codex",
    wire: Wire::Codex,
    label: "Codex",
    command: Some("codex-acp"),
    package: Some("@agentclientprotocol/codex-acp"),
    entry: Some("dist/index.js"),
    install: Some(Install {
        version: "2.8.0",
        node: "20.3",
        needs: None,
        download_mb: None,
        timeout_s: 120,
        // Codex was the first adapter offered.
        order: 1,
        home_variable: Some("CODEX_HOME"),
    }),
    shim: Some(Shim::Moved {
        legacy: "node_modules/@zed-industries/codex-acp/bin/codex-acp.js",
        legacy_marker: "@zed-industries",
        current_marker: "@agentclientprotocol",
    }),
    login: Some(LoginCommand {
        program: Some("codex"),
        args: &["login", "status"],
        reading: LoginReading::CodexLoginStatus,
    }),
    protocols: &[Protocol::Responses],
    ..BLANK
};

pub const PI: AgentSpec = AgentSpec {
    kind: "pi",
    wire: Wire::Pi,
    label: "Pi",
    command: Some("pi-acp"),
    package: Some("pi-acp"),
    entry: Some("dist/index.js"),
    // pi-acp starts `pi --mode rpc` from PATH. It answers ACP initialize
    // without Pi, so the install checks for Pi itself.
    install: Some(Install {
        version: "0.0.35",
        node: "20.0",
        needs: Some(Needed {
            command: "pi",
            version: "0.81",
            install: "npm install -g @earendil-works/pi-coding-agent",
        }),
        download_mb: None,
        timeout_s: 120,
        order: 3,
        home_variable: None,
    }),
    login: Some(LoginCommand {
        program: Some("pi"),
        args: &["--list-models"],
        reading: LoginReading::PiListModels,
    }),
    protocols: ALL_PROTOCOLS,
    ..BLANK
};

pub const DSH: AgentSpec = AgentSpec {
    kind: "dsh",
    wire: Wire::Dsh,
    label: "DeepSeek Harness",
    command: Some("dsh"),
    args: &["--profile", "acp"],
    package: Some("@deepseek-ai/dsh"),
    entry: Some("lib/bin.js"),
    // About 670 packages and 1.4 GB; a fresh install takes minutes.
    install: Some(Install {
        version: "0.2.2-rc.5",
        node: "24.9",
        needs: None,
        download_mb: Some(1400),
        timeout_s: 900,
        order: 4,
        home_variable: None,
    }),
    protocols: ALL_PROTOCOLS,
    // Keeps the official ACP registration apart from earlier headless DSH agents.
    setup_name: Some("dsh-acp"),
    ..BLANK
};

/// Any other ACP agent: its registration gives the command and arguments.
pub const OTHER_ACP: AgentSpec = AgentSpec {
    kind: "acp",
    wire: Wire::Acp,
    label: "ACP",
    ..BLANK
};

/// Every agent HarnessMesh knows, in the order it looks for them on PATH and
/// lists them in the panel. The generic ACP row comes last.
pub static CATALOG: &[&AgentSpec] = &[&OPENCODE, &CLAUDE_CODE, &CODEX, &PI, &DSH, &OTHER_ACP];

/// Which agent an Agent registration launches: one row of the catalog.
/// Written as the row's kind (`"pi"`, `"claude-code"`); the names of the
/// original six stay usable as `HarnessKind::Pi` and so on.
///
/// An agent card holds its agent's [`family`](Self::family), never a row
/// added later: other computers may run versions that know only the six.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HarnessKind(&'static AgentSpec);

#[allow(non_upper_case_globals)]
impl HarnessKind {
    /// OpenCode's built-in `opencode acp`.
    pub const Opencode: Self = Self(&OPENCODE);
    /// Claude Code through the `claude-agent-acp` adapter.
    pub const ClaudeCode: Self = Self(&CLAUDE_CODE);
    /// Codex through the `codex-acp` adapter.
    pub const Codex: Self = Self(&CODEX);
    /// Pi through the `pi-acp` adapter.
    pub const Pi: Self = Self(&PI);
    /// DeepSeek Harness's built-in `dsh --profile acp`.
    pub const Dsh: Self = Self(&DSH);
    /// Any other ACP agent; `adapter.args` gives its launch arguments.
    pub const Acp: Self = Self(&OTHER_ACP);

    pub const fn of(spec: &'static AgentSpec) -> Self {
        Self(spec)
    }
    pub fn spec(self) -> &'static AgentSpec {
        self.0
    }
    pub fn as_str(self) -> &'static str {
        self.0.kind
    }
    /// How other computers see this agent.
    pub fn wire(self) -> Wire {
        self.0.wire
    }
    /// The original kind this agent shows as on other computers, whose
    /// model-service settings and session handling it shares: itself for
    /// the original six, `Acp` for a row added later.
    pub fn family(self) -> Self {
        match self.0.wire {
            Wire::Pi => Self::Pi,
            Wire::Dsh => Self::Dsh,
            Wire::Opencode => Self::Opencode,
            Wire::ClaudeCode => Self::ClaudeCode,
            Wire::Codex => Self::Codex,
            Wire::Acp => Self::Acp,
        }
    }
    /// The catalog row with this kind.
    pub fn find(catalog: &[&'static AgentSpec], kind: &str) -> Option<Self> {
        catalog
            .iter()
            .copied()
            .find(|spec| spec.kind == kind)
            .map(Self)
    }
}
/// Equal kinds are equal rows: a catalog never holds a kind twice.
impl Hash for HarnessKind {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.kind.hash(state);
    }
}
impl fmt::Debug for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.kind)
    }
}
impl fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.kind)
    }
}
impl Serialize for HarnessKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.kind)
    }
}
static KINDS: LazyLock<&'static [&'static str]> = LazyLock::new(|| {
    CATALOG
        .iter()
        .map(|spec| spec.kind)
        .collect::<Vec<_>>()
        .leak()
});
impl<'de> Deserialize<'de> for HarnessKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kind = String::deserialize(deserializer)?;
        Self::find(CATALOG, &kind).ok_or_else(|| serde::de::Error::unknown_variant(&kind, *KINDS))
    }
}

/// How an agent card writes its agent's kind: always the family, so every
/// HarnessMesh version can read it. Use as `#[serde(with = "...")]`.
pub mod family {
    use super::HarnessKind;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(kind: &HarnessKind, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(kind.family().as_str())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<HarnessKind, D::Error> {
        HarnessKind::deserialize(deserializer).map(HarnessKind::family)
    }
}

/// Which catalog row a computer's agent comes from, told only to computers
/// that ask for it (`/hmesh/v1/agents/kinds`): the agent's card names its
/// family. Lists only agents whose row is not their family. Unknown fields
/// are ignored and an unknown kind is kept as text, so later versions can
/// add to it; `label` names a kind this version does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentKindNote {
    pub agent_id: Uuid,
    pub kind: String,
    pub label: String,
}
impl AgentKindNote {
    /// The note for an agent of `kind`, when its card's family does not say it.
    pub fn of(agent_id: Uuid, kind: HarnessKind) -> Option<Self> {
        (kind.family() != kind).then(|| Self {
            agent_id,
            kind: kind.as_str().into(),
            label: kind.spec().label.into(),
        })
    }
    /// A note from another computer worth showing: a short, printable kind
    /// and label.
    pub fn plausible(&self) -> bool {
        let short =
            |text: &str| text.len() <= MAX_KIND_NOTE_BYTES && !text.chars().any(char::is_control);
        !self.kind.is_empty() && short(&self.kind) && !self.label.is_empty() && short(&self.label)
    }
}
/// Longest kind or label a note may have, in bytes.
pub const MAX_KIND_NOTE_BYTES: usize = 100;

/// An ACP agent command found on this computer's PATH.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectedAgent {
    pub harness: HarnessKind,
    pub command: String,
    /// Absolute path as found on PATH (usable as `adapter.executable`).
    pub executable: PathBuf,
    /// Node.js to run it with, when that is not the first `node` on PATH
    /// (usable as `adapter.node_executable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<PathBuf>,
}

/// Look for the catalog's ACP commands in the absolute directories of `path`
/// (a `PATH` value), in catalog order. Nothing is run or installed.
pub fn detect_on_path(
    catalog: &[&'static AgentSpec],
    path: Option<OsString>,
) -> Vec<DetectedAgent> {
    let Some(paths) = path else {
        return vec![];
    };
    let directories: Vec<PathBuf> = std::env::split_paths(&paths)
        .filter(|p| p.is_absolute())
        .collect();
    let suffixes: &[&str] = if cfg!(windows) {
        &[".cmd", ".exe", ""]
    } else {
        &[""]
    };
    catalog
        .iter()
        .filter_map(|spec| {
            let command = spec.command?;
            directories.iter().find_map(|dir| {
                suffixes.iter().find_map(|suffix| {
                    let path = dir.join(format!("{command}{suffix}"));
                    path.is_file().then(|| DetectedAgent {
                        harness: HarnessKind::of(spec),
                        command: command.to_owned(),
                        executable: path,
                        node: None,
                    })
                })
            })
        })
        .collect()
}

/// The rows HarnessMesh can install, in install-list order.
pub fn installable(catalog: &[&'static AgentSpec]) -> Vec<&'static AgentSpec> {
    let mut rows: Vec<&'static AgentSpec> = catalog
        .iter()
        .copied()
        .filter(|spec| spec.install.is_some())
        .collect();
    rows.sort_by_key(|spec| spec.install.map(|install| install.order));
    rows
}

impl AgentSpec {
    /// Where a Windows npm shim next to `node_modules` finds this agent's
    /// current entry, without looking at the shim.
    pub fn shim_entry(&self) -> Option<String> {
        match self.shim {
            Some(Shim::At(entry)) => Some(entry.into()),
            _ => Some(format!("node_modules/{}/{}", self.package?, self.entry?)),
        }
    }
    /// The panel's onboarding dialog names it `label` and finds `command`.
    /// A row without a command is filled in by hand (`custom`).
    pub fn onboarding_view(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind,
            "label": self.label,
            "command": self.command.unwrap_or(""),
            "custom": self.command.is_none(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_consistent() {
        let mut kinds: Vec<_> = CATALOG.iter().map(|spec| spec.kind).collect();
        kinds.sort();
        kinds.dedup();
        assert_eq!(kinds.len(), CATALOG.len(), "a kind is listed twice");
        for spec in CATALOG {
            let kind = HarnessKind::of(spec);
            assert!(!spec.kind.is_empty() && !spec.label.is_empty());
            // The family is the row of the same wire kind, written as before.
            assert_eq!(kind.family().wire(), spec.wire);
            assert!(CATALOG.contains(&kind.family().spec()));
            if kind.family() != kind {
                assert!(
                    spec.protocols.is_empty(),
                    "{}: settings code exists only per wire kind",
                    spec.kind
                );
            }
            if spec.install.is_some() {
                assert!(
                    spec.command.is_some() && spec.package.is_some() && spec.entry.is_some(),
                    "{}",
                    spec.kind
                );
            }
        }
    }

    #[test]
    fn the_original_kinds_are_written_as_before() {
        let written: Vec<String> = [
            HarnessKind::Pi,
            HarnessKind::Dsh,
            HarnessKind::Opencode,
            HarnessKind::ClaudeCode,
            HarnessKind::Codex,
            HarnessKind::Acp,
        ]
        .iter()
        .map(|kind| {
            serde_json::to_value(kind)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
        assert_eq!(
            written,
            ["pi", "dsh", "opencode", "claude-code", "codex", "acp"]
        );
        for kind in &written {
            let read: HarnessKind = serde_json::from_value(serde_json::json!(kind)).unwrap();
            assert_eq!(read.to_string(), *kind);
            assert_eq!(read.family(), read);
        }
        assert!(serde_json::from_value::<HarnessKind>(serde_json::json!("kimi")).is_err());
        assert!(serde_json::from_value::<HarnessKind>(serde_json::json!("Pi")).is_err());
    }

    #[test]
    fn shim_entries_are_the_ones_used_before_the_catalog() {
        let entries: Vec<_> = CATALOG
            .iter()
            .map(|spec| (spec.kind, spec.shim_entry()))
            .collect();
        assert_eq!(
            entries,
            [
                (
                    "opencode",
                    Some("node_modules/opencode-ai/bin/opencode.exe".into())
                ),
                (
                    "claude-code",
                    Some("node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js".into())
                ),
                (
                    "codex",
                    Some("node_modules/@agentclientprotocol/codex-acp/dist/index.js".into())
                ),
                ("pi", Some("node_modules/pi-acp/dist/index.js".into())),
                (
                    "dsh",
                    Some("node_modules/@deepseek-ai/dsh/lib/bin.js".into())
                ),
                ("acp", None),
            ]
        );
    }

    #[test]
    fn status_commands_are_the_ones_run_before_the_catalog() {
        let commands: Vec<_> = CATALOG
            .iter()
            .filter_map(|spec| Some((spec.kind, spec.login?.program, spec.login?.args)))
            .collect();
        assert_eq!(
            commands,
            [
                ("opencode", None, &["auth", "list"][..]),
                ("codex", Some("codex"), &["login", "status"][..]),
                ("pi", Some("pi"), &["--list-models"][..]),
            ]
        );
    }

    #[test]
    fn launch_defaults_and_setup_names_are_the_ones_used_before_the_catalog() {
        let defaults: Vec<_> = CATALOG.iter().map(|spec| (spec.kind, spec.args)).collect();
        assert_eq!(
            defaults,
            [
                ("opencode", &["acp"][..]),
                ("claude-code", &[][..]),
                ("codex", &[][..]),
                ("pi", &[][..]),
                ("dsh", &["--profile", "acp"][..]),
                ("acp", &[][..]),
            ]
        );
        let protocols: Vec<_> = CATALOG.iter().map(|spec| spec.protocols.len()).collect();
        assert_eq!(protocols, [3, 1, 1, 3, 3, 0]);
        assert_eq!(DSH.setup_name, Some("dsh-acp"));
        assert!(
            CATALOG
                .iter()
                .filter(|spec| spec.kind != "dsh")
                .all(|spec| spec.setup_name.is_none())
        );
    }

    #[test]
    fn install_lists_keep_codex_first() {
        let kinds: Vec<_> = installable(CATALOG).iter().map(|spec| spec.kind).collect();
        assert_eq!(kinds, ["codex", "claude-code", "pi", "dsh"]);
    }
}
