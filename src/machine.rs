use anyhow::{Context, Result};
use serde::Deserialize;

use crate::ext::Ext;
use crate::model::{Agent, AgentView, MachineId, TerminalId};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Core {
    pub id: MachineId,
    pub name: String,
}

/// One entry of `hc machine ls --json`. `name` is what this PC calls the machine; another PC
/// may call it something else.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Machine {
    pub id: MachineId,
    pub name: String,
    #[serde(default)]
    pub host: Option<String>,
    pub state: String,
}

impl Machine {
    pub fn connected(&self) -> bool {
        self.state == "connected"
    }
}

pub fn core(ext: &Ext) -> Result<Core> {
    #[derive(Deserialize)]
    struct Status {
        core: Core,
    }
    let text = ext
        .hc(["server", "status", "--json"])?
        .ok()
        .context("이 PC의 hc 코어 id를 묻지 못했다")?;
    let status: Status =
        serde_json::from_str(&text).context("hc server status의 출력을 읽지 못했다")?;
    Ok(status.core)
}

pub fn list(ext: &Ext) -> Result<Vec<Machine>> {
    #[derive(Deserialize)]
    struct Listing {
        machines: Vec<Machine>,
    }
    let text = ext
        .hc(["machine", "ls", "--json"])?
        .ok()
        .context("hc가 아는 머신 목록을 묻지 못했다")?;
    let listing: Listing =
        serde_json::from_str(&text).context("hc machine ls의 출력을 읽지 못했다")?;
    Ok(listing.machines)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machines {
    local: Core,
    others: Vec<Machine>,
}

impl Machines {
    pub fn new(local: Core, others: Vec<Machine>) -> Machines {
        Machines { local, others }
    }

    pub fn load(ext: &Ext) -> Result<Machines> {
        Ok(Machines::new(core(ext)?, list(ext)?))
    }

    pub fn local(&self) -> &Core {
        &self.local
    }

    pub fn by_id(&self, id: &MachineId) -> Option<&Machine> {
        self.others.iter().find(|m| m.id == *id)
    }

    pub fn by_name(&self, name: &str) -> Option<&Machine> {
        self.others.iter().find(|m| m.name == name)
    }

    pub fn address(&self, machine: &MachineId, terminal: &TerminalId) -> TerminalId {
        match self.by_id(machine) {
            Some(m) if *machine != self.local.id => {
                TerminalId(format!("{}/{}", m.name, terminal.0))
            }
            _ => terminal.clone(),
        }
    }

    pub fn agent_view(&self, home: &MachineId, agent: &Agent) -> AgentView {
        let id = agent.machine.as_ref().unwrap_or(home);
        let (host, machine) = if *id == self.local.id {
            (Some(self.local.name.clone()), None)
        } else {
            match self.by_id(id) {
                Some(m) => (m.host.clone(), Some(m.name.clone())),
                None => (None, None),
            }
        };
        AgentView {
            terminal: self.address(id, &agent.terminal),
            name: agent.name.clone(),
            alive: agent.alive,
            machine_id: Some(id.clone()),
            host,
            machine,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EpochMs;
    use crate::testkit::{Reply, Sandbox};

    fn id(s: &str) -> MachineId {
        MachineId::try_from(s.to_owned()).unwrap()
    }

    fn machines() -> Machines {
        let other = |s: &str, name: &str, host: &str| Machine {
            id: id(s),
            name: name.into(),
            host: Some(host.into()),
            state: "connected".into(),
        };
        Machines::new(
            Core {
                id: id("aaa"),
                name: "hpg1a".into(),
            },
            vec![
                other("bbb", "desk", "desk-host"),
                other("ccc", "lap", "lap-host"),
            ],
        )
    }

    #[test]
    fn addresses_are_written_from_the_viewing_pc() {
        let m = machines();
        let t7 = TerminalId("t7".into());
        assert_eq!(m.address(&id("aaa"), &t7).0, "t7");
        assert_eq!(m.address(&id("bbb"), &t7).0, "desk/t7");
        assert_eq!(m.address(&id("ccc"), &t7).0, "lap/t7");
        assert_eq!(m.address(&id("zzz"), &t7).0, "t7");
    }

    #[test]
    fn names_change_with_the_viewer_but_ids_do_not() {
        let viewer = Machines::new(
            Core {
                id: id("bbb"),
                name: "desk-host".into(),
            },
            vec![Machine {
                id: id("aaa"),
                name: "home".into(),
                host: None,
                state: "connected".into(),
            }],
        );
        let t7 = TerminalId("t7".into());
        assert_eq!(viewer.address(&id("aaa"), &t7).0, "home/t7");
        assert_eq!(viewer.address(&id("bbb"), &t7).0, "t7");
    }

    #[test]
    fn an_agent_without_a_machine_lives_on_the_state_s_own_machine() {
        let agent = |machine: Option<&str>| Agent {
            terminal: TerminalId("t7".into()),
            created_ms: EpochMs(1),
            name: Some("impl".into()),
            workdir: "/w".into(),
            alive: true,
            machine: machine.map(id),
        };
        let m = machines();
        let home = id("aaa");
        let terminals: Vec<String> = [None, Some("bbb"), Some("zzz")]
            .into_iter()
            .map(|machine| m.agent_view(&home, &agent(machine)).terminal.0)
            .collect();
        assert_eq!(terminals, ["t7", "desk/t7", "t7"]);
        let here = m.agent_view(&home, &agent(None));
        assert_eq!(
            (here.machine_id, here.host, here.machine),
            (Some(id("aaa")), Some("hpg1a".into()), None)
        );
        let there = m.agent_view(&home, &agent(Some("bbb")));
        assert_eq!(
            (there.host, there.machine),
            (Some("desk-host".into()), Some("desk".into()))
        );
        let unknown = m.agent_view(&home, &agent(Some("zzz")));
        assert_eq!(
            (unknown.machine_id, unknown.host, unknown.machine),
            (Some(id("zzz")), None, None)
        );
    }

    #[test]
    fn reads_the_core_and_the_machine_list_from_hc() {
        let sb = Sandbox::new();
        let hc = sb.fake("hc");
        hc.clear_rules().respond(
            "server status --json",
            Reply::ok(r#"{"core":{"id":"aaa","name":"hpg1a"},"live":6}"#),
        );
        hc.respond(
            "machine ls --json",
            Reply::ok(
                r#"{"machines":[{"id":"bbb","name":"desk","host":"desk-host","state":"needs_login"},
                                {"id":"ccc","name":"lap","state":"connected"}]}"#,
            ),
        );
        let ext = Ext::from_vars(|k| sb.var(k));
        let m = Machines::load(&ext).unwrap();
        assert_eq!(m.local().name, "hpg1a");
        assert!(!m.by_name("desk").unwrap().connected());
        assert!(m.by_id(&id("ccc")).unwrap().connected());
        assert_eq!(m.by_id(&id("ccc")).unwrap().host, None);
        assert!(m.by_name("nosuch").is_none());
    }

    #[test]
    fn ids_cannot_hide_a_slash_or_blank() {
        for bad in ["", " ", "a/b", "a b", "a\n"] {
            assert!(MachineId::try_from(bad.to_owned()).is_err(), "{bad:?}");
        }
    }
}
