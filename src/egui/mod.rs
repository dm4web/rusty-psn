use bytesize::ByteSize;
use eframe::{egui, epi};
use poll_promise::Promise;

use tokio::sync::mpsc;
use tokio::runtime::Runtime;
use tokio::io::AsyncWriteExt;

use crate::utils;
use crate::psn::{DownloadError, UpdateError, UpdateInfo, PackageInfo};

pub struct ActiveDownload {
    id: String,
    version: String,

    download_size: u64,
    download_progress: u64,

    download_promise: Promise<Result<(), DownloadError>>,
    download_progress_rx: mpsc::Receiver<u64>
}

pub struct UpdatesApp {
    rt: Runtime,

    serial_query: String,
    update_results: Vec<UpdateInfo>,

    error_msg: String,
    show_error_window: bool,

    download_queue: Vec<ActiveDownload>,
    failed_downloads: Vec<(String, String)>,
    completed_downloads: Vec<(String, String)>,

    search_promise: Option<Promise<Result<UpdateInfo, UpdateError>>>
}

impl Default for UpdatesApp {
    fn default() -> UpdatesApp {
        UpdatesApp {
            rt: Runtime::new().unwrap(),

            serial_query: String::new(),
            update_results: Vec::new(),

            error_msg: String::new(),
            show_error_window: false,

            download_queue: Vec::new(),
            failed_downloads: Vec::new(),
            completed_downloads: Vec::new(),

            search_promise: None
        }
    }
}

impl epi::App for UpdatesApp {
    fn name(&self) -> &str {
        "rusty-psn"
    }

    fn update(&mut self, ctx: &egui::CtxRef, frame: &epi::Frame) {
        egui::CentralPanel::default().show(ctx, | ui | {
            ui.horizontal(| ui | {
                ui.label("Title Serial:");
                let input_submitted = ui.text_edit_singleline(&mut self.serial_query).lost_focus() && ui.input().key_pressed(egui::Key::Enter);

                ui.separator();
                
                ui.add_enabled_ui(!self.serial_query.is_empty() && self.search_promise.is_none(), | ui | {
                    if (input_submitted || ui.button("Search for updates").clicked()) && !self.update_results.iter().any(|e| e.title_id == self.serial_query) {
                        let _guard = self.rt.enter();
                        let promise = Promise::spawn_async(UpdateInfo::get_info(self.serial_query.clone()));
                        
                        self.search_promise = Some(promise);
                    }
                });

                ui.add_enabled_ui(!self.update_results.is_empty(), | ui | {
                    if ui.button("Clear results").clicked() {
                        self.update_results = Vec::new();
                    }
                });
            });

            ui.separator();

            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, | ui | {
                let mut new_downloads = Vec::new();

                for update in self.update_results.iter() {
                    let collapsing_title = {
                        if let Some(last_pkg) = update.tag.packages.last() {
                            if let Some(param) = last_pkg.paramsfo.as_ref() {
                                format!("{} - {}", update.title_id.clone(), param.titles[0])
                            }
                            else {
                                update.title_id.clone()
                            }
                        }
                        else {
                            update.title_id.clone()
                        }
                    };
    
                    ui.collapsing(collapsing_title, | ui | {
                        let total_updates_size = {
                            let mut size = 0;

                            for pkg in update.tag.packages.iter() {
                                size += pkg.size.parse::<u64>().unwrap_or(0);
                            }

                            size
                        };

                        if ui.button(format!("Download all ({})", ByteSize::b(total_updates_size))).clicked() {
                            for pkg in update.tag.packages.iter() {
                                if !self.download_queue.iter().any(| d | d.id == update.title_id && d.version == pkg.version) {
                                    self.start_download(update.title_id.clone(), pkg, &mut new_downloads);
                                }
                            }
                        }

                        ui.separator();

                        for pkg in update.tag.packages.iter() {
                            let bytes = pkg.size.parse::<u64>().unwrap_or(0);
                                
                            ui.strong(format!("Package Version: {}", pkg.version));
                            ui.label(format!("Size: {}", ByteSize::b(bytes)));
                            ui.label(format!("SHA-1 hashsum: {}", pkg.sha1sum));

                            ui.horizontal(| ui | {
                                let download = self.download_queue.iter().find(| d | d.id == update.title_id && d.version == pkg.version);

                                if ui.add_enabled(download.is_none(), egui::Button::new("Download file")).clicked() {
                                    self.start_download(update.title_id.clone(), pkg, &mut new_downloads);
                                }

                                if let Some(download) = download {
                                    let progress = egui::ProgressBar::new(download.download_progress as f32 / download.download_size as f32)
                                        .show_percentage()
                                    ;

                                    ui.add(progress);
                                }
                                else if self.completed_downloads.iter().any(| (id, version) | id == &update.title_id && version == &pkg.version) {
                                    ui.label(egui::RichText::new("Completed").color(egui::color::Rgba::from_rgb(0.0, 1.0, 0.0)));
                                }
                                else if self.failed_downloads.iter().any(| (id, version) | id == &update.title_id && version == &pkg.version) {
                                    ui.label(egui::RichText::new("Failed").color(egui::color::Rgba::from_rgb(1.0, 0.0, 0.0)));
                                }
                            });

                            ui.separator();
                        }
                    });
                }

                for dl in new_downloads {
                    self.download_queue.push(dl);
                }
            });
        });

        if !self.error_msg.is_empty() && self.show_error_window {
            let label = self.error_msg.clone();
            // There was an attempt to properly center it :)
            let position = ctx.available_rect().center();
            let mut acknowledged = false;

            egui::Window::new("An error ocurred").collapsible(false).open(&mut self.show_error_window).resizable(false).default_pos(position).show(ctx, | ui | {
                ui.label(label);

                if ui.button("Ok").clicked() {
                    acknowledged = true;
                }
            });

            if acknowledged {
                self.show_error_window = false;
                self.error_msg = String::new();
            }
        }

        if let Some(promise) = self.search_promise.as_ref() {
            if let Some(result) = promise.ready() {
                if let Ok(update_info) = result {
                    self.update_results.push(update_info.clone());
                }
                else if let Err(e) = result {
                    self.show_error_window = true;

                    match e {
                        UpdateError::Serde => {
                            self.error_msg = "Error parsing response from Sony, try again later.".to_string();
                        }
                        UpdateError::InvalidSerial => {
                            self.error_msg = "The provided serial didn't give any results, double-check your input.".to_string();
                        }
                        UpdateError::NoUpdatesAvailable => {
                            self.error_msg = "The provided serial doesn't have any available updates.".to_string();
                        }
                        UpdateError::Reqwest(e) => {
                            self.error_msg = format!("There was an error on the request: {}", e);
                        }
                    }
                }
                
                self.search_promise = None;
            }
        }

        let mut entries_to_remove = Vec::new();

        for (i, download) in self.download_queue.iter_mut().enumerate() {
            if let Ok(progress) = download.download_progress_rx.try_recv() {
                download.download_progress += progress;
            }

            if let Some(r) = download.download_promise.ready() {
                entries_to_remove.push(i);

                match r {
                    Ok(_) => {
                        self.completed_downloads.push((download.id.clone(), download.version.clone()));
                    }
                    Err(e) => {
                        self.show_error_window = true;
                        self.failed_downloads.push((download.id.clone(), download.version.clone()));

                        match e {
                            DownloadError::HashMismatch => {
                                self.error_msg = format!("There was an error downloading the {} update file for {}: The hash for the downloaded file doesn't match.", download.version, download.id);
                            }
                            DownloadError::Tokio(e) => {
                                self.error_msg = format!("There was an error downloading the {} update file for {}: {e}", download.version, download.id);
                            }
                            DownloadError::Reqwest(e) => {
                                self.error_msg = format!("There was an error downloading the {} update file for {}: {e}", download.version, download.id);
                            }
                        }
                    }
                }
            }
        }

        for (removed_entries, entry) in entries_to_remove.into_iter().enumerate() {
            self.download_queue.remove(entry - removed_entries);
        }

        frame.request_repaint();
    }
}

impl UpdatesApp {
    fn start_download(&self, title_id: String, pkg: &PackageInfo, downloads_queue: &mut Vec<ActiveDownload>) {
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let serial = title_id.clone();

        let pkg_url = pkg.url.clone();
        let pkg_size = pkg.size.clone();
        let pkg_hash = pkg.sha1sum.clone();

        let _guard = self.rt.enter();

        let download_promise = Promise::spawn_async(async move {
            let tx = tx;

            let pkg_url = pkg_url;
            let pkg_size = pkg_size;
            let pkg_hash = pkg_hash;

            let (file_name, mut response) = utils::send_pkg_request(pkg_url).await?;
            let mut file = utils::create_pkg_file(std::path::PathBuf::from(format!("pkgs/{}/{}", serial, file_name))).await?;

            if !utils::hash_file(&mut file, &pkg_hash).await? {
                file.set_len(0).await.map_err(DownloadError::Tokio)?;

                while let Some(download_chunk) = response.chunk().await.map_err(DownloadError::Reqwest)? {
                    let download_chunk = download_chunk.as_ref();
    
                    tx.send(download_chunk.len() as u64).await.unwrap();
                    file.write_all(download_chunk).await.map_err(DownloadError::Tokio)?;
                }
                                                
                if utils::hash_file(&mut file, &pkg_hash).await? {
                    Ok(())
                }
                else {
                    Err(DownloadError::HashMismatch)
                }
            }
            else {
                tx.send(pkg_size.parse().unwrap_or(0)).await.unwrap();
                Ok(())
            }
        });

        let dl = ActiveDownload {
            id: title_id,
            version: pkg.version.clone(),

            download_size: pkg.size.parse().unwrap_or(0),
            download_progress: 0,

            download_promise,
            download_progress_rx: rx
        };

        downloads_queue.push(dl);
    }
}