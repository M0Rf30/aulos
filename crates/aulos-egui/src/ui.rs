//! Views: library panel (Albums / Songs + search), now-playing area and the
//! transport bar.

use super::{App, Msg};
use aulos_core::config::RepeatMode;
use aulos_core::library::state::unfilter_index;
use aulos_core::library::{Album, Track};
use aulos_core::session::PlaybackUpdate;
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Albums,
    Songs,
}

/// A user intent collected while the library borrows `self.session`.
enum Action {
    PlayAlbum(usize),
    PlayTrack(usize),
}

const ROW_HEIGHT: f32 = 52.0;

impl App {
    pub fn show(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("transport")
            .exact_size(86.0)
            .show(ui, |ui| self.transport_bar(ui));
        egui::Panel::left("library")
            .default_size(460.0)
            .size_range(320.0..=800.0)
            .show(ui, |ui| self.library_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| self.now_playing(ui));
    }

    // -- Library ----------------------------------------------------------

    fn library_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Albums, "Albums");
            ui.selectable_value(&mut self.tab, Tab::Songs, "Songs");
        });
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search…")
                .desired_width(f32::INFINITY),
        );
        if search.changed() {
            let library = &mut self.session.library;
            library.search_query = self.search.clone();
            library.search_debounce_gen += 1;
            if self.search.trim().is_empty() {
                // Clearing is cheap and should feel instant.
                self.refresh_search();
            } else {
                let pending = self.session.library.search_debounce_gen;
                let effect = aulos_core::task::Effect::future(async move {
                    tokio::time::sleep(super::SEARCH_DEBOUNCE).await;
                    pending
                });
                self.runner.run(effect, Msg::SearchDebounced);
            }
        }
        ui.separator();

        if self.session.library.scanning {
            ui.label("Scanning…");
        }
        let action = match self.tab {
            Tab::Albums => self.album_list(ui),
            Tab::Songs => self.song_table(ui),
        };
        let update = match action {
            Some(Action::PlayAlbum(i)) => self.session.play_album(i, 0),
            Some(Action::PlayTrack(i)) => self.session.play_library_track(i),
            None => return,
        };
        self.apply_playback(update);
    }

    fn album_list(&self, ui: &mut egui::Ui) -> Option<Action> {
        let lib = &self.session.library;
        // The same "is the filter live" predicate the COSMIC view uses.
        let (albums, map): (&[Album], Option<&[usize]>) = if lib.search_query.trim().is_empty() {
            (&lib.albums, None)
        } else {
            (&lib.filtered.albums, Some(&lib.filtered.album_map))
        };
        if albums.is_empty() {
            ui.label(if lib.scanning { "" } else { "No albums" });
            return None;
        }
        let mut action = None;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(
            ui,
            ROW_HEIGHT,
            albums.len(),
            |ui, range| {
                for i in range {
                    let album = &albums[i];
                    let response = ui
                        .horizontal(|ui| {
                            ui.set_height(ROW_HEIGHT - 4.0);
                            match self.covers.get(&album.cover_key) {
                                Some(cover) => {
                                    ui.add(
                                        egui::Image::new(&cover.texture)
                                            .fit_to_exact_size(egui::vec2(46.0, 46.0)),
                                    );
                                }
                                None => {
                                    ui.add_space(46.0 + ui.spacing().item_spacing.x);
                                }
                            }
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&album.name).strong());
                                ui.label(format!(
                                    "{} · {} · {} tracks",
                                    album.artist,
                                    album.year,
                                    album.tracks.len()
                                ));
                            });
                        })
                        .response
                        .interact(egui::Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if response.clicked() {
                        action = Some(Action::PlayAlbum(unfilter_index(map, i)));
                    }
                }
            },
        );
        action
    }

    fn song_table(&self, ui: &mut egui::Ui) -> Option<Action> {
        let lib = &self.session.library;
        let (tracks, map): (&[Track], Option<&[usize]>) = if lib.search_query.trim().is_empty() {
            (&lib.tracks, None)
        } else {
            (&lib.filtered.tracks, Some(&lib.filtered.track_map))
        };
        let current = self.session.playback.current_track.as_ref().map(|t| t.id);
        let mut action = None;
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .column(Column::remainder().clip(true))
            .column(Column::initial(110.0).clip(true))
            .column(Column::exact(44.0))
            .header(20.0, |mut header| {
                for title in ["Title", "Artist", "Time"] {
                    header.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, tracks.len(), |mut row| {
                    let track = &tracks[row.index()];
                    row.col(|ui| {
                        let title = RichText::new(&track.title);
                        ui.label(if Some(track.id) == current {
                            title.strong()
                        } else {
                            title
                        });
                    });
                    row.col(|ui| {
                        ui.label(&track.artist);
                    });
                    row.col(|ui| {
                        ui.label(track.duration_string());
                    });
                    if row.response().double_clicked() {
                        action = Some(Action::PlayTrack(unfilter_index(map, row.index())));
                    }
                });
            });
        action
    }

    // -- Now playing ------------------------------------------------------

    fn now_playing(&self, ui: &mut egui::Ui) {
        let Some(track) = &self.session.playback.current_track else {
            let lib = &self.session.library;
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
                ui.heading("Aulos");
                ui.label(format!(
                    "{} tracks · {} albums · {} artists",
                    lib.tracks.len(),
                    lib.albums.len(),
                    lib.artists.len()
                ));
                if !lib.music_dirs_present {
                    ui.label("No music folder configured or present.");
                }
            });
            return;
        };
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            let key =
                aulos_core::library::CoverArt::album_key(track.grouping_artist(), &track.album);
            if let Some(cover) = self.covers.get(&key) {
                ui.add(
                    egui::Image::new(&cover.texture)
                        .fit_to_exact_size(egui::vec2(256.0, 256.0))
                        .corner_radius(6.0),
                );
            }
            ui.add_space(12.0);
            ui.heading(&track.title);
            ui.label(&track.artist);
            ui.label(format!("{} ({})", track.album, track.year));
            if track.bitrate > 0 {
                ui.weak(format!(
                    "{} kbps · {} Hz",
                    track.bitrate / 1000,
                    track.sample_rate
                ));
            }
        });
    }

    // -- Transport bar ----------------------------------------------------

    fn transport_bar(&mut self, ui: &mut egui::Ui) {
        let mut update: Option<PlaybackUpdate> = None;
        let playing = self.session.playback.is_playing();
        let (title, artist, duration) = match &self.session.playback.current_track {
            Some(t) => (t.title.clone(), t.artist.clone(), t.duration),
            None => (
                "Nothing playing".to_owned(),
                String::new(),
                Default::default(),
            ),
        };

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong());
            ui.label(artist);
            if !self.status.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(&self.status);
                });
            }
        });

        ui.horizontal(|ui| {
            if ui.button("Prev").clicked() {
                update = Some(self.session.previous_track());
            }
            if ui.button(if playing { "Pause" } else { "Play" }).clicked() {
                update = Some(self.session.toggle_playback());
            }
            if ui.button("Next").clicked() {
                update = Some(self.session.next_track());
            }
            if ui.button("Stop").clicked() {
                update = Some(self.session.stop());
            }

            let shuffle = self.session.config.config.shuffle;
            if ui.selectable_label(shuffle, "Shuffle").clicked() {
                update = Some(self.session.toggle_shuffle());
                self.session.config.save();
            }
            let repeat = match self.session.config.config.repeat_mode {
                RepeatMode::None => "Repeat: off",
                RepeatMode::All => "Repeat: all",
                RepeatMode::One => "Repeat: one",
            };
            if ui.button(repeat).clicked() {
                update = Some(self.session.cycle_repeat());
                self.session.config.save();
            }

            // Volume, pinned right.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut volume = self
                    .session
                    .playback
                    .player
                    .as_ref()
                    .map_or(self.session.config.config.volume, |p| p.volume());
                let slider = ui.add(
                    egui::Slider::new(&mut volume, 0.0..=1.0)
                        .show_value(false)
                        .text("Vol"),
                );
                if slider.changed() {
                    update = Some(self.session.set_volume(volume));
                    self.volume_dirty = true;
                }
                if self.volume_dirty && !slider.is_pointer_button_down_on() {
                    self.session.commit_volume();
                    self.volume_dirty = false;
                }
            });
        });

        // Seek bar: `seek_preview` is set while dragging so the periodic tick
        // doesn't fight the thumb; the seek happens on release.
        let position = self.session.playback.position;
        let fraction = match duration.as_secs_f32() {
            d if d > 0.0 => (position.as_secs_f32() / d).clamp(0.0, 1.0),
            _ => 0.0,
        };
        ui.horizontal(|ui| {
            ui.monospace(aulos_core::format::format_duration(position.as_secs()));
            let mut value = self.seek_preview.unwrap_or(fraction);
            ui.spacing_mut().slider_width = (ui.available_width() - 64.0).max(50.0);
            let slider = ui.add_enabled(
                duration.as_secs() > 0,
                egui::Slider::new(&mut value, 0.0..=1.0).show_value(false),
            );
            if slider.changed() {
                self.seek_preview = Some(value);
            }
            if let Some(target) = self.seek_preview
                && !slider.is_pointer_button_down_on()
            {
                self.seek_preview = None;
                update = Some(self.session.seek_fraction(target));
            }
            ui.monospace(aulos_core::format::format_duration(duration.as_secs()));
        });

        if let Some(update) = update {
            self.apply_playback(update);
        }
    }
}
