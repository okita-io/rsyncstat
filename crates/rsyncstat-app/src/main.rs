//! RSyncStat M0: single-root scanner + treemap viewer.

mod ui;

use crossbeam_channel::{unbounded, Receiver};
use eframe::egui;
use rsyncstat_filter::FilterSet;
use rsyncstat_model::TreeNode;
use rsyncstat_scan::scan_root_async;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("RSyncStat"),
        ..Default::default()
    };
    eframe::run_native(
        "RSyncStat",
        native_options,
        Box::new(|cc| Ok(Box::new(RsyncStatApp::new(cc)))),
    )
}

struct RsyncStatApp {
    root: Option<PathBuf>,
    filter: Arc<FilterSet>,
    tree: Option<TreeNode>,
    scanning: bool,
    scan_rx: Option<Receiver<std::io::Result<TreeNode>>>,
    status: String,
    selected_path: Option<Vec<String>>,
    hover_path: Option<Vec<String>>,
}

impl RsyncStatApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            root: None,
            filter: Arc::new(FilterSet::default_presets()),
            tree: None,
            scanning: false,
            scan_rx: None,
            status: "Choose a folder to scan.".to_string(),
            selected_path: None,
            hover_path: None,
        }
    }

    fn pick_folder(&mut self) {
        let folder = rfd::FileDialog::new().pick_folder();
        if let Some(path) = folder {
            self.start_scan(path);
        }
    }

    fn start_scan(&mut self, path: PathBuf) {
        self.root = Some(path.clone());
        self.scanning = true;
        self.tree = None;
        self.status = format!("Scanning {}…", path.display());
        let (tx, rx) = unbounded();
        self.scan_rx = Some(rx);
        let filter = Arc::clone(&self.filter);
        thread::spawn(move || scan_root_async(path, filter, tx));
    }

    fn poll_scan(&mut self) {
        let Some(rx) = self.scan_rx.as_ref() else {
            return;
        };
        if let Ok(result) = rx.try_recv() {
            self.scanning = false;
            self.scan_rx = None;
            match result {
                Ok(tree) => {
                    let bytes = tree.total_size();
                    self.status = format!(
                        "Scan complete — {} bytes indexed (excluded branches pruned).",
                        format_bytes(bytes)
                    );
                    self.tree = Some(tree);
                }
                Err(e) => {
                    self.status = format!("Scan failed: {e}");
                }
            }
        }
    }
}

impl eframe::App for RsyncStatApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scan();
        if self.scanning {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let tree = self.tree.clone();
        let root_path = self.root.clone();

        egui::Panel::top("menu").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open folder…").clicked() {
                    self.pick_folder();
                }
                ui.separator();
                if ui.button("Rescan").clicked() {
                    if let Some(path) = self.root.clone() {
                        self.start_scan(path);
                    }
                }
                ui.separator();
                ui.label(
                    "Filters: default built-in presets (*.lancedb, node_modules/, target/, …)",
                );
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
                if let Some(path) = &self.hover_path {
                    if let Some(node) = self.tree.as_ref().and_then(|t| find_by_path(t, path)) {
                        ui.separator();
                        let rule = node.exclude_rule.as_deref().unwrap_or("(built-in preset)");
                        if node.excluded {
                            ui.label(format!(
                                "Hover: {} — excluded ({rule}), not measured",
                                path.join("/")
                            ));
                        } else {
                            ui.label(format!(
                                "Hover: {} — {}",
                                path.join("/"),
                                format_bytes(node.size)
                            ));
                        }
                    }
                }
            });
        });

        egui::Panel::left("tree_panel")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                ui.heading("Directory tree");
                ui.separator();
                if let Some(ref t) = tree {
                    let root_path = vec![t.name.clone()];
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui::tree_view_with_path(
                            ui,
                            t,
                            &root_path,
                            &mut self.selected_path,
                            &mut self.hover_path,
                        );
                    });
                } else if self.scanning {
                    ui.spinner();
                    ui.label("Scanning…");
                } else {
                    ui.label("No data yet.");
                }

                ui.separator();
                ui.heading("File types");
                if let Some(ref t) = tree {
                    ui::type_legend(ui, t);
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Treemap");
            if let (Some(ref t), Some(ref root)) = (&tree, &root_path) {
                ui::treemap_view(ui, t, &mut self.selected_path, &mut self.hover_path, root);
            } else if self.scanning {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("Open a folder to build the treemap.");
                });
            }
        });
    }
}

fn find_by_path<'a>(root: &'a TreeNode, path: &[String]) -> Option<&'a TreeNode> {
    if path.first()? != &root.name {
        return None;
    }
    let mut node = root;
    for part in path.iter().skip(1) {
        node = node.children.iter().find(|c| &c.name == part)?;
    }
    Some(node)
}

fn format_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n >= K * K * K {
        format!("{:.2} GiB", n / (K * K * K))
    } else if n >= K * K {
        format!("{:.2} MiB", n / (K * K))
    } else if n >= K {
        format!("{:.2} KiB", n / K)
    } else {
        format!("{n:.0} B")
    }
}
