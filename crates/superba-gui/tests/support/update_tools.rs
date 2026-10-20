//! Explicit fake inventory, used only by tests and the screenshot gallery.
#![allow(dead_code)]
use std::collections::BTreeMap;
use std::sync::Mutex;
use superba_gui::pages::Cx;
use superba_gui::pages::superba::updates::{
    UpdateItem, UpdateStatus,
    items::{Entry, Operation, Progress},
    tools::{Removal, ToolId, ToolRow, ToolsSource, ToolsView},
};

#[derive(Debug)]
pub struct DemoTools {
    states: Mutex<BTreeMap<ToolId, UpdateStatus>>,
    pub started: Mutex<Vec<ToolId>>,
    pub cancelled: Mutex<Vec<ToolId>>,
    pub checks: Mutex<usize>,
    pub selected_versions: Mutex<Vec<(ToolId, String)>>,
    pub removals: Mutex<Vec<(ToolId, bool)>>,
}
impl Default for DemoTools {
    fn default() -> Self {
        use UpdateStatus::*;
        Self {
            states: Mutex::new(
                ToolId::ALL
                    .into_iter()
                    .zip([
                        Available, Blocked, Current, Installing, Failed, Vendor, Missing,
                    ])
                    .collect(),
            ),
            started: Mutex::new(vec![]),
            cancelled: Mutex::new(vec![]),
            checks: Mutex::new(0),
            selected_versions: Mutex::new(vec![]),
            removals: Mutex::new(vec![]),
        }
    }
}
impl DemoTools {
    pub fn set(&self, id: ToolId, status: UpdateStatus) {
        self.states.lock().unwrap().insert(id, status);
    }
    pub fn idle(&self) {
        self.set(ToolId::Pi, UpdateStatus::Current);
    }
}
impl ToolsSource for DemoTools {
    fn view(&self, cx: &Cx<'_>) -> ToolsView {
        let states = self.states.lock().unwrap();
        let rows = ToolId::ALL
            .into_iter()
            .map(|id| {
                let status = states[&id];
                let (old, new) = match id {
                    ToolId::ClaudeCode => ("2.1.284", "2.1.295"),
                    ToolId::Dsh => ("0.2.0-rc.2", "0.2.0-rc.3"),
                    ToolId::Opencode => ("2.0.32", "2.0.32"),
                    ToolId::Pi => ("", "1.3.0"),
                    ToolId::Zcode => ("3.14.4", "3.15.3"),
                    ToolId::Workbuddy => ("5.8.3", "5.8.3"),
                    ToolId::WorkbuddyAi => ("", "5.8.4"),
                };
                let zh = cx.tr.t("updates.list.update") == "更新";
                let mut entry = Entry::new(UpdateItem::Tool(id), id.name(), status);
                entry.current = (!old.is_empty()).then(|| old.into());
                entry.latest = (status != UpdateStatus::Blocked).then(|| new.into());
                entry.description = match status {
                    UpdateStatus::Blocked => "DISABLE_UPDATES=1".into(),
                    UpdateStatus::Failed => if zh {
                        "安装程序退出（代码 2）：请关闭 ZCode 后重试"
                    } else {
                        "Installer exited (code 2): close ZCode and retry"
                    }
                    .into(),
                    UpdateStatus::Vendor => if zh {
                        "由应用自动更新"
                    } else {
                        "The app updates itself"
                    }
                    .into(),
                    _ => format!(
                        "{} · ~/.local/bin/{}",
                        if zh { "独立安装" } else { "Standalone" },
                        id.as_str()
                    ),
                };
                entry.operation = match status {
                    UpdateStatus::Available | UpdateStatus::Missing | UpdateStatus::Failed => {
                        Some(Operation::Tool(id))
                    }
                    UpdateStatus::Installing => Some(Operation::Cancel),
                    UpdateStatus::Unknown => Some(Operation::Check),
                    _ => None,
                };
                if status == UpdateStatus::Installing {
                    entry.progress = Some(Progress {
                        fraction: Some(18.4 / 31.0),
                        label: if zh {
                            "正在下载 18.4 / 31.0 MB · 校验 sha512"
                        } else {
                            "Downloading 18.4 / 31.0 MB · sha512 verification"
                        }
                        .into(),
                    });
                }
                entry.cautions = vec![
                    if zh {
                        "更新前请结束正在运行的任务。"
                    } else {
                        "Finish running tasks before updating."
                    }
                    .into(),
                ];
                ToolRow {
                    searched: vec![],
                    install_method: None,
                    choose_path: id.has_command(),
                    command_path: None,
                    entry,
                    install_cautions: vec![],
                    installs: vec![format!("~/.local/bin/{} · PATH #1", id.as_str())],
                    channels: vec!["stable".into(), "latest".into()],
                    channel: "stable".into(),
                    managed: Some(true),
                    notes: vec![],
                    log: Some("/example/logs/tool-install.log".into()),
                    versions: vec![new.into(), "1.0.0".into()],
                    removal: (!old.is_empty()).then(|| Removal {
                        cautions: vec![
                            if zh {
                                "卸载工具将停止其后续任务。"
                            } else {
                                "Uninstalling prevents new tasks from using this tool."
                            }
                            .into(),
                        ],
                        data_dirs: vec![format!("~/.config/{}", id.as_str())],
                    }),
                }
            })
            .collect();
        ToolsView {
            rows,
            ..Default::default()
        }
    }
    fn check(&self) {
        *self.checks.lock().unwrap() += 1;
    }
    fn start(&self, id: ToolId) -> Result<(), String> {
        assert!(
            !self
                .states
                .lock()
                .unwrap()
                .values()
                .any(|s| *s == UpdateStatus::Installing),
            "operations must not overlap"
        );
        self.started.lock().unwrap().push(id);
        self.set(id, UpdateStatus::Installing);
        Ok(())
    }
    fn cancel(&self, id: ToolId) {
        self.cancelled.lock().unwrap().push(id);
    }
    fn install_version(&self, id: ToolId, version: String) -> Result<(), String> {
        self.selected_versions.lock().unwrap().push((id, version));
        self.start(id)
    }
    fn remove(&self, id: ToolId, remove_data: bool) -> Result<(), String> {
        self.removals.lock().unwrap().push((id, remove_data));
        self.start(id)
    }
    fn channel(&self, _: ToolId, _: String) -> Result<(), String> {
        Ok(())
    }
    fn managed(&self, _: ToolId, _: bool) -> Result<(), String> {
        Ok(())
    }
}
