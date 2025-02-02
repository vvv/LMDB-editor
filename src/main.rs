#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

use std::mem;
use std::ops::Deref;

use crate::escaped_entry::EscapedEntry;
use eframe::egui::{self, InnerResponse};
use egui::Color32;
use egui_extras::{Column, TableBuilder};
use egui_tiles::Container;
use egui_tiles::Tile;
use either::Either;
use heed::types::ByteSlice;
use heed::{Database, Env, EnvOpenOptions};
use heed::{RoTxn, RwTxn};
use once_cell::sync::OnceCell;
use rfd::FileDialog;

mod escaped_entry;

static ENV: OnceCell<Env> = OnceCell::new();

fn main() -> anyhow::Result<()> {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

    let options = eframe::NativeOptions {
        initial_window_size: Some(egui::vec2(720.0, 480.0)),
        ..Default::default()
    };

    if let Some(env_path) = FileDialog::new().pick_folder() {
        let env = EnvOpenOptions::new().max_dbs(1000).open(env_path)?;
        let _ = ENV.set(env);

        eframe::run_native("LMDB Editor", options, Box::new(|ctx| Box::new(LmdbEditor::new(ctx))))
            .unwrap();
    }

    Ok(())
}

struct LmdbEditor {
    txn: Either<RoTxn<'static>, RwTxn<'static>>,
    tree: egui_tiles::Tree<Pane>,
}

impl LmdbEditor {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // Customize egui here with cc.egui_ctx.set_fonts and cc.egui_ctx.set_visuals.
        // Restore app state using cc.storage (requires the "persistence" feature).
        // Use the cc.gl (a glow::Context) to create graphics shaders and buffers that you can use
        // for e.g. egui::PaintCallback.

        // TODO do not try to create the database here.
        let env = ENV.wait();
        let mut wtxn = env.write_txn().unwrap();
        let main_db = env.create_database(&mut wtxn, None).unwrap();
        wtxn.commit().unwrap();

        let mut tiles = egui_tiles::Tiles::default();
        let mut tabs = vec![];

        tabs.push(tiles.insert_pane(Pane::DatabaseEntries {
            database_name: None,
            database: main_db,
            entry_to_insert: EscapedEntry::default(),
        }));
        tabs.push(tiles.insert_pane(Pane::OpenNew { database_to_open: String::new() }));
        let root = tiles.insert_tab_tile(tabs);
        let tree = egui_tiles::Tree::new(root, tiles);

        let rtxn = env.read_txn().unwrap();
        LmdbEditor { txn: Either::Left(rtxn), tree }
    }
}

impl eframe::App for LmdbEditor {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                let env = ENV.wait();
                let button = if self.txn.is_right() {
                    egui::Button::new("currently writing").fill(Color32::GREEN)
                } else {
                    egui::Button::new("currently reading").fill(Color32::RED)
                };

                if ui.add(button).clicked() && self.txn.is_left() {
                    let wtxn = env.write_txn().unwrap();
                    self.txn = Either::Right(wtxn);
                }

                if ui.button("commit changes").clicked() {
                    if let Some(wtxn) =
                        replace_right_with(&mut self.txn, || env.read_txn().unwrap())
                    {
                        wtxn.commit().unwrap();
                    }
                }

                if ui.button("abort changes").clicked() {
                    if let Some(wtxn) =
                        replace_right_with(&mut self.txn, || env.read_txn().unwrap())
                    {
                        wtxn.abort();
                    }
                }
            });

            let LmdbEditor { txn, tree } = self;

            let mut behavior = TreeBehavior { txn: txn.as_mut() };
            tree.ui(&mut behavior, ui);

            // Automatically insert an OpenNew Tab when one is missing
            if let Some(root) = self.tree.root() {
                let must_insert = match self.tree.tiles.get(root).unwrap() {
                    Tile::Container(Container::Tabs(t)) => t
                        .children
                        .iter()
                        .find(|&&t| {
                            self.tree
                                .tiles
                                .get(t)
                                .map_or(true, |t| matches!(t, Tile::Pane(p) if p.is_open_new()))
                        })
                        .is_none(),
                    _ => false,
                };

                if must_insert {
                    let tid = self
                        .tree
                        .tiles
                        .insert_pane(Pane::OpenNew { database_to_open: String::new() });
                    if let Tile::Container(Container::Tabs(t)) =
                        self.tree.tiles.get_mut(root).unwrap()
                    {
                        t.children.push(tid);
                    }
                }
            }
        });
    }
}

fn replace_right_with<L, R, F: FnMut() -> L>(either: &mut Either<L, R>, mut f: F) -> Option<R> {
    match either {
        Either::Left(_) => None,
        Either::Right(_) => {
            let obvious_right = mem::replace(either, Either::Left(f()));
            match obvious_right {
                Either::Left(_) => unreachable!(),
                Either::Right(right) => Some(right),
            }
        }
    }
}

enum Pane {
    DatabaseEntries {
        database_name: Option<String>,
        database: Database<ByteSlice, ByteSlice>,
        entry_to_insert: EscapedEntry,
    },
    OpenNew {
        database_to_open: String,
    },
}

impl Pane {
    fn is_open_new(&self) -> bool {
        matches!(self, Pane::OpenNew { .. })
    }
}

struct TreeBehavior<'a> {
    txn: Either<&'a mut RoTxn<'static>, &'a mut RwTxn<'static>>,
}

impl egui_tiles::Behavior<Pane> for TreeBehavior<'_> {
    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        match pane {
            Pane::DatabaseEntries { database_name: Some(name), .. } => format!("{name}").into(),
            Pane::DatabaseEntries { database_name: None, .. } => format!("{{main}}").into(),
            Pane::OpenNew { .. } => format!("Open new").into(),
        }
    }

    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: egui_tiles::TileId,
        pane: &mut Pane,
    ) -> egui_tiles::UiResponse {
        ui.add_space(5.0);

        match pane {
            Pane::DatabaseEntries { database, entry_to_insert, database_name, .. } => {
                let name = database_name.as_ref().map_or_else(|| "{main}".to_owned(), Clone::clone);
                egui::Window::new(format!("Put an entry into {name}")).default_pos([720.0, 480.0]).show(ui.ctx(), |ui| {
                    ui.style_mut().spacing.interact_size.y = 0.0; // hack to make `horizontal_wrapped` work better with text.

                    ui.label("We use STFU-8 as a hacky text encoding/decoding protocol for data that might be not quite UTF-8 but is still mostly UTF-8. \
                    It is based on the syntax of the repr created when you write (or print) binary text in python, C or other common programming languages.");

                    ui.add_space(8.0);

                    ui.label("Basically STFU-8 is the text format you already write when use escape codes in C, python, rust, etc. \
                    It permits binary data in UTF-8 by escaping them with \\, for instance \\n and \\x0F.");

                    ui.add_space(8.0);

                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label("More about how we interpret encoding/decoding ");
                        ui.hyperlink_to("on the stfu8 documentation", "https://docs.rs/stfu8");
                        ui.label(".");
                    });

                    ui.separator();

                    let EscapedEntry { key, data } = entry_to_insert;
                    ui.add(egui::TextEdit::singleline(key).hint_text("escaped key"));
                    ui.add(egui::TextEdit::multiline(data).hint_text("escaped data"));

                    if ui.button("insert").clicked() {
                        if let Either::Right(wtxn) = self.txn.as_mut() {
                            let key = entry_to_insert.decoded_key().unwrap();
                            let data = entry_to_insert.decoded_data().unwrap();
                            database.put(wtxn, &key, &data).unwrap();
                            entry_to_insert.clear();
                        }
                    }

                    if ui.button("delete").clicked() {
                        if let Either::Right(wtxn) = self.txn.as_mut() {
                            let key = entry_to_insert.decoded_key().unwrap();
                            database.delete(wtxn, &key).unwrap();
                            entry_to_insert.clear();
                        }
                    }
                });

                // If there is a write txn opened, use it, else make the wtxn live longer and deref it.
                let long_wtxn: &&mut RwTxn;
                let rtxn: &heed::RoTxn;
                match self.txn.as_ref() {
                    Either::Left(txn) => rtxn = txn,
                    Either::Right(wtxn) => {
                        long_wtxn = wtxn;
                        rtxn = long_wtxn.deref();
                    }
                };

                let num_rows = database.len(&rtxn).unwrap().try_into().unwrap();
                let mut prev_row_index = None;
                let mut iter = database.iter(&rtxn).unwrap();

                TableBuilder::new(ui)
                    .column(Column::auto().resizable(true))
                    .column(Column::auto().resizable(true))
                    .column(Column::remainder())
                    .header(20.0, |mut header| {
                        header.col(|ui| {
                            ui.label("Keys");
                        });
                        header.col(|ui| {
                            ui.label("Values");
                        });
                        header.col(|ui| {
                            ui.label("Operations");
                        });
                    })
                    .body(|body| {
                        body.rows(30.0, num_rows, |row_index, mut row| {
                            assert!(prev_row_index.map_or(true, |p| p + 1 == row_index));
                            if prev_row_index.is_none() {
                                iter.by_ref().take(row_index).for_each(drop);
                            }
                            prev_row_index = Some(row_index);

                            if let Some(result) = iter.next() {
                                let (key, data) = result.unwrap();
                                let encoded_key = stfu8::encode_u8_pretty(key);
                                let encoded_data = stfu8::encode_u8_pretty(data);

                                row.col(|ui| {
                                    ui.label(&encoded_key);
                                });
                                row.col(|ui| {
                                    ui.label(&encoded_data);
                                });
                                row.col(|ui| {
                                    // TODO Replace me by a ✏️
                                    if ui.button("edit").clicked() {
                                        entry_to_insert.key = encoded_key;
                                        entry_to_insert.data = encoded_data;
                                    }
                                    // // Replace me by a red 🗑️
                                    // if ui.button("delete").clicked() {
                                    //     if let Some(wtxn) = self.wtxn.as_mut() {
                                    //     }
                                    // }
                                });
                            }
                        });
                    });
            }
            Pane::OpenNew { database_to_open } => {
                let response = ui.horizontal(|ui| {
                    // If there is a write txn opened, use it, else make the wtxn live longer and deref it.
                    let long_wtxn: &&mut RwTxn;
                    let rtxn: &heed::RoTxn;
                    match self.txn.as_ref() {
                        Either::Left(txn) => rtxn = txn,
                        Either::Right(wtxn) => {
                            long_wtxn = wtxn;
                            rtxn = long_wtxn.deref();
                        }
                    };

                    ui.add(egui::TextEdit::singleline(database_to_open).hint_text("database name"));
                    if ui.button("open").clicked() {
                        let env = ENV.wait();
                        let database_name = if database_to_open.is_empty() {
                            None
                        } else {
                            Some(mem::take(database_to_open))
                        };

                        let database = env
                            .open_database(&rtxn, database_name.as_ref().map(AsRef::as_ref))
                            .unwrap();

                        match database {
                            Some(database) => Some(Pane::DatabaseEntries {
                                database_name,
                                database,
                                entry_to_insert: Default::default(),
                            }),
                            None => None,
                        }
                    } else {
                        None
                    }
                });

                if let InnerResponse { inner: Some(p), .. } = response {
                    *pane = p;
                }
            }
        }

        egui_tiles::UiResponse::None
    }
}
