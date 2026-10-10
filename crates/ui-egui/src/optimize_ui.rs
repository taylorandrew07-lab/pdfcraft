//! Optimize PDF ▸ Advanced optimization (Acrobat's PDF Optimizer): Images, Discard Objects,
//! Discard User Data and Clean Up panels. The result is saved as a copy, like Reduce File Size.
//!
//! The optimization (this dialog's, or Reduce File Size's fixed choices) runs on a worker thread
//! and shows its progress in a notice with a bar and a Cancel button; the copy is saved once it
//! is done.

use std::sync::{Arc, Mutex};

use egui::{Align, Layout};
use pdfcraft_engine::optimize::{Compression, ImageSettings, QUALITIES, ReducePreset, Settings};
use pdfcraft_engine::optimizer::{OptimizeJob, OptimizeStage, Optimized};
use pdfcraft_engine::{DocId, EditError, Hidden, OptimizeReport};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptimizeTab {
    Images,
    DiscardObjects,
    DiscardUserData,
    CleanUp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptimizeDraft {
    pub tab: OptimizeTab,
    pub settings: Settings,
    /// Remove Hidden Information categories to discard.
    pub discard: Vec<Hidden>,
    /// "Audit space usage…" was pressed.
    pub audit: bool,
}

impl Default for OptimizeDraft {
    fn default() -> Self {
        Self { tab: OptimizeTab::Images, settings: Settings::default(), discard: Vec::new(), audit: false }
    }
}

/// Progress of a background optimization: the stage, the result once finished, and a cancel
/// request.
#[derive(Default)]
pub struct OptimizeProgress {
    pub stage: OptimizeStage,
    pub result: Option<Result<Optimized, EditError>>,
    pub cancel: bool,
}

/// Which command started an optimization: they save under different names, and only the
/// Optimizer's notice lists what it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptimizeKind {
    /// Reduce File Size with one of its choices.
    Reduce(ReducePreset),
    /// Optimize PDF ▸ Advanced optimization (the dialog's choices).
    Advanced,
}

impl OptimizeKind {
    /// The suffix of the saved copy's name ("notes (reduced).pdf").
    pub(crate) fn suffix(self) -> &'static str {
        match self {
            OptimizeKind::Reduce(_) => "reduced",
            OptimizeKind::Advanced => "optimized",
        }
    }
}

pub struct OptimizeRun {
    pub doc: DocId,
    pub kind: OptimizeKind,
    pub progress: Arc<Mutex<OptimizeProgress>>,
}

/// The notice's text for a stage.
fn stage_label(stage: OptimizeStage) -> String {
    match stage {
        OptimizeStage::Discarding => tl!("Optimizing… removing user data").to_string(),
        OptimizeStage::Images { done, total } => crate::i18n::fmt(
            tl!("Optimizing… image {d} of {t}"),
            &[("d", &(done + 1).min(total.max(1)).to_string()), ("t", &total.max(1).to_string())],
        ),
        OptimizeStage::CleaningUp => tl!("Optimizing… cleaning up").to_string(),
        OptimizeStage::Merging => tl!("Optimizing… merging identical objects").to_string(),
        OptimizeStage::Writing => tl!("Optimizing… writing the copy").to_string(),
    }
}

/// What the saved notice adds after the size: images optimized and items discarded.
fn summary(r: &OptimizeReport) -> String {
    let o = &r.optimize;
    let mut parts = Vec::new();
    if o.images_resampled + o.images_recompressed > 0 {
        parts.push(format!(
            "{} image{} optimized",
            o.images_resampled + o.images_recompressed,
            if o.images_resampled + o.images_recompressed == 1 { "" } else { "s" }
        ));
    }
    let discarded: usize = r.discarded.iter().map(|(_, n)| n).sum();
    if discarded > 0 {
        parts.push(format!("{discarded} item{} discarded", if discarded == 1 { "" } else { "s" }));
    }
    if parts.is_empty() { String::new() } else { format!("; {}", parts.join(", ")) }
}

fn image_row(ui: &mut egui::Ui, id: &str, title: &str, s: &mut ImageSettings) {
    ui.label(egui::RichText::new(tl!(title)).font(theme::semibold(13.0)));
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.downsample, tl!("Bicubic downsampling to"));
        ui.add_enabled(s.downsample, egui::DragValue::new(&mut s.target_ppi).range(9.0..=2400.0).suffix(" ppi"));
        ui.label(tl!("for images above"));
        ui.add_enabled(s.downsample, egui::DragValue::new(&mut s.above_ppi).range(9.0..=2400.0).suffix(" ppi"));
    });
    s.above_ppi = s.above_ppi.max(s.target_ppi);
    ui.horizontal(|ui| {
        ui.label(tl!("Compression"));
        let label = match s.compression {
            Compression::Jpeg(_) => "JPEG",
            Compression::Flate => tl!("ZIP"),
            Compression::Retain => tl!("Retain existing"),
        };
        egui::ComboBox::from_id_salt((id, "compression")).selected_text(label).show_ui(ui, |ui| {
            let q = match s.compression {
                Compression::Jpeg(q) => q,
                _ => 60,
            };
            ui.selectable_value(&mut s.compression, Compression::Jpeg(q), "JPEG");
            ui.selectable_value(&mut s.compression, Compression::Flate, tl!("ZIP"));
            ui.selectable_value(&mut s.compression, Compression::Retain, tl!("Retain existing"));
        });
        if let Compression::Jpeg(q) = &mut s.compression {
            ui.label(tl!("Quality"));
            let name = QUALITIES.iter().min_by_key(|(_, v)| (*v as i32 - *q as i32).abs()).map_or(tl!("Medium"), |(n, _)| tl!(n));
            egui::ComboBox::from_id_salt((id, "quality")).selected_text(name).show_ui(ui, |ui| {
                for (n, v) in QUALITIES {
                    ui.selectable_value(q, v, tl!(n));
                }
            });
        }
    });
    ui.add_space(8.0);
}

fn discard_box(ui: &mut egui::Ui, list: &mut Vec<Hidden>, h: Hidden, label: &str) {
    let mut on = list.contains(&h);
    if ui.checkbox(&mut on, tl!(label)).changed() {
        if on {
            list.push(h);
        } else {
            list.retain(|x| *x != h);
        }
    }
}

/// Draw the dialog; returns (ok, cancel).
pub(crate) fn body(ui: &mut egui::Ui, d: &mut OptimizeDraft, t: &Tokens) -> (bool, bool) {
    ui.label(egui::RichText::new(tl!("PDF Optimizer")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        for (tab, label) in [
            (OptimizeTab::Images, tl!("Images")),
            (OptimizeTab::DiscardObjects, tl!("Discard Objects")),
            (OptimizeTab::DiscardUserData, tl!("Discard User Data")),
            (OptimizeTab::CleanUp, tl!("Clean Up")),
        ] {
            if widgets::mode_tab(ui, label, d.tab == tab).clicked() {
                d.tab = tab;
            }
        }
    });
    ui.separator();
    ui.add_space(6.0);
    let s = &mut d.settings;
    match d.tab {
        OptimizeTab::Images => {
            image_row(ui, "color", "Color Images", &mut s.color);
            image_row(ui, "gray", "Grayscale Images", &mut s.gray);
            ui.label(
                egui::RichText::new(tl!("Each image is measured where pages draw it; an image is replaced only if the result is smaller."))
                    .small()
                    .color(t.text_muted),
            );
        }
        OptimizeTab::DiscardObjects => {
            discard_box(ui, &mut d.discard, Hidden::LinksActionsScripts, "Discard all links, actions and JavaScript");
            ui.checkbox(&mut s.discard_alternate_images, tl!("Discard alternate images"));
            ui.checkbox(&mut s.discard_thumbnails, tl!("Discard embedded page thumbnails"));
            ui.checkbox(&mut s.discard_tags, tl!("Discard document tags"));
            ui.checkbox(&mut s.discard_print_settings, tl!("Discard embedded print settings"));
            discard_box(ui, &mut d.discard, Hidden::Bookmarks, "Discard bookmarks");
            discard_box(ui, &mut d.discard, Hidden::FormFields, "Flatten form fields");
            discard_box(ui, &mut d.discard, Hidden::HiddenLayers, "Discard hidden layer content");
        }
        OptimizeTab::DiscardUserData => {
            discard_box(ui, &mut d.discard, Hidden::Comments, "Discard all comments, forms and multimedia");
            discard_box(ui, &mut d.discard, Hidden::Metadata, "Discard document information and metadata");
            discard_box(ui, &mut d.discard, Hidden::Attachments, "Discard all object data (file attachments)");
            discard_box(ui, &mut d.discard, Hidden::PrivateData, "Discard private data of other applications");
            discard_box(ui, &mut d.discard, Hidden::HiddenText, "Discard hidden text");
        }
        OptimizeTab::CleanUp => {
            ui.checkbox(&mut s.flate_unencoded, tl!("Use Flate to encode streams that are not encoded"));
            ui.checkbox(&mut s.remove_invalid_links, tl!("Remove invalid links and bookmarks"));
            ui.checkbox(&mut s.remove_unreferenced_dests, tl!("Remove unreferenced named destinations"));
            ui.add_enabled(false, egui::Checkbox::new(&mut true, tl!("Compress document structure (object streams)")));
            ui.add_enabled(false, egui::Checkbox::new(&mut true, tl!("Remove unused objects and merge identical ones")));
        }
    }
    ui.add_space(12.0);
    let (mut ok, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Audit space usage…"), false).clicked() {
            d.audit = true;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("OK"), true).clicked() {
                ok = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                cancel = true;
            }
        });
    });
    (ok, cancel)
}

/// What the Reduce file size dialog was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReduceAction {
    Reduce,
    /// Open the PDF Optimizer with the chosen preset's settings.
    Advanced,
    Cancel,
}

/// A choice's name, what it is for, and what it does.
fn preset_text(p: ReducePreset) -> (&'static str, &'static str, &'static str) {
    match p {
        ReducePreset::Smallest => ("Smallest", "For email and screens", "Images at 100 ppi, lower quality"),
        ReducePreset::Balanced => ("Balanced", "Good for most documents", "Images at 150 ppi, medium quality"),
        ReducePreset::HighQuality => ("High quality", "For printing", "Images at 225 ppi, high quality"),
    }
}

/// Reduce file size: the document's size now, the three choices (with their estimated sizes,
/// `None` where there are no estimates) and Advanced….
pub(crate) fn reduce_body(
    ui: &mut egui::Ui,
    preset: &mut ReducePreset,
    name: &str,
    size: usize,
    estimates: Option<[Estimate; 3]>,
    t: &Tokens,
) -> Option<ReduceAction> {
    ui.label(egui::RichText::new(tl!("Reduce file size")).font(theme::semibold(18.0)));
    ui.add_space(4.0);
    let now = crate::i18n::fmt(tl!("{name} · {size} now"), &[("name", name), ("size", &crate::panels::human_size(size))]);
    ui.label(egui::RichText::new(now).color(t.text_muted));
    ui.add_space(10.0);
    for (i, p) in ReducePreset::ALL.into_iter().enumerate() {
        let (label, purpose, detail) = preset_text(p);
        ui.horizontal(|ui| {
            ui.radio_value(preset, p, egui::RichText::new(tl!(label)).strong());
            let estimate = match estimates.as_ref().and_then(|e| e.get(i)) {
                Some(Estimate::Pending) => tl!("estimating…").to_string(),
                Some(Estimate::Size(n)) if *n < size => crate::i18n::fmt(tl!("about {size}"), &[("size", &crate::panels::human_size(*n))]),
                Some(Estimate::Size(_)) => tl!("no smaller").to_string(),
                Some(Estimate::Failed) | None => String::new(),
            };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(egui::RichText::new(estimate).color(t.text_muted)));
        });
        ui.indent(("reduce-preset", p.id()), |ui| {
            ui.label(tl!(purpose));
            ui.label(egui::RichText::new(tl!(detail)).small().color(t.text_muted));
        });
        ui.add_space(6.0);
    }
    ui.add_space(12.0);
    let mut action = None;
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Advanced…"), false).clicked() {
            action = Some(ReduceAction::Advanced);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("Reduce"), true).clicked() {
                action = Some(ReduceAction::Reduce);
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                action = Some(ReduceAction::Cancel);
            }
        });
    });
    action
}

/// One choice's estimate in the Reduce file size dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Estimate {
    Pending,
    /// The reduced copy's size in bytes.
    Size(usize),
    Failed,
}

/// Up to this size the estimated copies are kept, so Reduce saves the chosen one at once; larger
/// documents keep only the sizes (Reduce then reduces again), to bound memory.
const KEEP_ESTIMATES: usize = 64 << 20;

/// Above this size no estimates are made: three reductions of a very large file take long and
/// need a lot of memory, while the dialog is often open only for a moment.
const ESTIMATE_UP_TO: usize = 256 << 20;

/// What the estimate worker has found so far.
#[derive(Default)]
pub struct EstimateProgress {
    /// Per choice, in [`ReducePreset::ALL`] order: the size, or `None` if it failed.
    sizes: [Option<Option<usize>>; 3],
    /// The copies themselves, when kept.
    kept: [Option<Optimized>; 3],
    cancel: bool,
    /// The worker hasn't finished (it notices `cancel` only between the steps of a reduction).
    running: bool,
}

/// Reduce file size's estimates for one version of a document, computed in the background.
pub struct ReduceEstimates {
    doc: DocId,
    /// The document's edit generation they were made for: after an edit they are stale.
    generation: u64,
    progress: Arc<Mutex<EstimateProgress>>,
    /// Still offered to the dialog. Stopped estimates are kept only while their worker
    /// finishes, so that a second worker never starts beside it.
    live: bool,
}

impl ReduceEstimates {
    /// The estimates for document `doc` as of `generation`, in [`ReducePreset::ALL`] order:
    /// `None` when these are stopped, or for another document or version.
    pub(crate) fn current(&self, doc: DocId, generation: u64) -> Option<[Estimate; 3]> {
        if !self.is_for(doc, generation) {
            return None;
        }
        let Ok(s) = self.progress.lock() else { return Some([Estimate::Failed; 3]) };
        Some(s.sizes.map(|size| match size {
            None => Estimate::Pending,
            Some(Some(n)) => Estimate::Size(n),
            Some(None) => Estimate::Failed,
        }))
    }

    /// Live estimates for document `doc` as of `generation`.
    pub(crate) fn is_for(&self, doc: DocId, generation: u64) -> bool {
        self.live && self.doc == doc && self.generation == generation
    }

    /// Still offered to the dialog.
    pub fn live(&self) -> bool {
        self.live
    }

    fn running(&self) -> bool {
        self.progress.lock().is_ok_and(|s| s.running)
    }
}

/// Reduce the document with each job (an index into [`ReducePreset::ALL`] and the job) in turn,
/// recording the sizes (and the copies, if `keep`), until cancelled.
fn estimate(progress: &Mutex<EstimateProgress>, jobs: Vec<(usize, OptimizeJob)>, keep: bool, ctx: Option<&egui::Context>) {
    for (i, job) in jobs {
        let result = job.run(|_| progress.lock().is_ok_and(|s| !s.cancel));
        let Ok(mut s) = progress.lock() else { return };
        if s.cancel {
            return;
        }
        match result {
            Ok((bytes, report)) => {
                if let Some(size) = s.sizes.get_mut(i) {
                    *size = Some(Some(bytes.len()));
                }
                if keep && let Some(k) = s.kept.get_mut(i) {
                    *k = Some((bytes, report));
                }
            }
            Err(_) => {
                if let Some(size) = s.sizes.get_mut(i) {
                    *size = Some(None);
                }
            }
        }
        drop(s);
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    }
}

impl PdfCraftApp {
    /// Start estimating the three choices for the active document as it is now (not on the web,
    /// which has no worker threads, and not for very large files). Replaces earlier estimates;
    /// while a stopped worker is still finishing, none are started.
    pub(crate) fn start_reduce_estimates(&mut self) {
        self.stop_reduce_estimates();
        if cfg!(target_arch = "wasm32") || self.reduce_estimates.is_some() {
            return;
        }
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let (size, generation) = (doc.bytes.len(), doc.edit_generation());
        if size > ESTIMATE_UP_TO {
            return;
        }
        // The preselected choice first: it is the likeliest to be reduced.
        let first = self.reduce_preset;
        let order = std::iter::once(first).chain(ReducePreset::ALL.into_iter().filter(|p| *p != first));
        let jobs: Vec<(usize, OptimizeJob)> = order
            .filter_map(|p| {
                let i = ReducePreset::ALL.iter().position(|q| *q == p)?;
                self.session.optimize_job(id, &Settings::reduce(p), &[]).ok().map(|job| (i, job))
            })
            .collect();
        // A document Reduce refuses (signed, read-only) shows no estimates.
        if jobs.len() != ReducePreset::ALL.len() {
            return;
        }
        let progress = Arc::new(Mutex::new(EstimateProgress { running: true, ..EstimateProgress::default() }));
        let (p, ctx, keep) = (progress.clone(), self.ctx.clone(), size <= KEEP_ESTIMATES);
        let work = move || {
            estimate(&p, jobs, keep, ctx.as_ref());
            if let Ok(mut s) = p.lock() {
                s.running = false;
            }
        };
        #[cfg(target_arch = "wasm32")]
        drop(work);
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            work();
        } else if std::thread::Builder::new().name("pdfcraft-reduce-estimates".into()).spawn(work).is_err() {
            // No worker: no estimates, Reduce still works.
            return;
        }
        self.reduce_estimates = Some(ReduceEstimates { doc: id, generation, progress, live: true });
    }

    /// Stop the estimate worker and drop what it found. The entry stays, no longer live, until
    /// the worker has finished.
    pub(crate) fn stop_reduce_estimates(&mut self) {
        let Some(e) = self.reduce_estimates.as_mut() else { return };
        e.live = false;
        if let Ok(mut s) = e.progress.lock() {
            s.cancel = true;
            s.kept = Default::default();
        }
        if !e.running() {
            self.reduce_estimates = None;
        }
    }

    /// The estimated copy for `preset` of document `id`, if it is ready, was kept, and the
    /// document hasn't changed since.
    pub(crate) fn take_reduce_estimate(&mut self, id: DocId, preset: ReducePreset) -> Option<Optimized> {
        let generation = self.session.get(id)?.edit_generation();
        let e = self.reduce_estimates.as_ref().filter(|e| e.is_for(id, generation))?;
        let i = ReducePreset::ALL.iter().position(|p| *p == preset)?;
        e.progress.lock().ok()?.kept.get_mut(i)?.take()
    }
}

/// Above this a copy is likely too big to email: 25 MB is a common attachment limit.
pub(crate) const EMAIL_LIMIT: usize = 25 << 20;

/// What the saved notice adds for Reduce file size: images left as they are, and a hint when the
/// file is still too big to email (the copy, or the original when the copy isn't smaller and so
/// isn't saved). Starts with ". " when not empty.
pub(crate) fn reduce_detail(r: &OptimizeReport, after: usize, before: usize, preset: ReducePreset) -> String {
    let mut parts = Vec::new();
    let unsupported = r.optimize.images_unsupported as u64;
    if unsupported > 0 {
        parts.push(crate::i18n::trn(
            crate::i18n::current(),
            unsupported,
            "{n} image was left as it is: PdfCraft can't recompress its format yet.",
            "{n} images were left as they are: PdfCraft can't recompress their format yet.",
        ));
    }
    if after.min(before) > EMAIL_LIMIT {
        parts.push(match preset {
            ReducePreset::Smallest => tl!("Still over 25 MB, the usual email limit.").to_string(),
            _ => tl!("Still over 25 MB, the usual email limit. Try Smallest.").to_string(),
        });
    }
    if parts.is_empty() { String::new() } else { format!(". {}", parts.join(" ")) }
}

/// Audit Space Usage: bytes and share of the file per kind of content.
pub(crate) fn audit_body(ui: &mut egui::Ui, rows: &[pdfcraft_engine::optimize::SpaceUse], t: &Tokens) -> bool {
    ui.label(egui::RichText::new(tl!("Space Audit")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("space-audit").num_columns(3).striped(true).spacing([24.0, 4.0]).show(ui, |ui| {
        for h in [tl!("Description"), tl!("Bytes"), tl!("Percentage")] {
            ui.label(egui::RichText::new(h).color(t.text_muted));
        }
        ui.end_row();
        let total: u64 = rows.iter().map(|r| r.bytes).sum();
        for r in rows.iter().filter(|r| r.bytes > 0) {
            // "Patterns" also names the redaction search patterns ("模式"); the audit's PDF
            // graphics objects need their own key.
            let category = if r.category == pdfcraft_engine::optimize::SpaceCategory::Patterns {
                tl!("Patterns (graphics objects)").to_string()
            } else {
                tl!(r.category.label()).to_string()
            };
            ui.label(category);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(r.bytes.to_string()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(format!("{:.2}%", r.percent)));
            ui.end_row();
        }
        ui.label(egui::RichText::new(tl!("Total")).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(egui::RichText::new(total.to_string()).strong()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(egui::RichText::new("100.00%").strong()));
        ui.end_row();
    });
    ui.add_space(12.0);
    let mut ok = false;
    ui.horizontal(|ui| ui.with_layout(Layout::right_to_left(Align::Center), |ui| ok = widgets::pill_button(ui, tl!("OK"), true).clicked()));
    ok
}

impl PdfCraftApp {
    /// Optimize PDF with the dialog's choices on a worker thread; the copy is saved when done.
    pub fn optimize_with_draft(&mut self) {
        let (settings, discard) = (self.optimize_draft.settings.clone(), self.optimize_draft.discard.clone());
        self.start_optimize(OptimizeKind::Advanced, &settings, &discard);
    }

    /// Run an optimization of the active document on a worker thread (inline in tests and on
    /// the web), showing its progress; the copy is saved when it is done.
    pub(crate) fn start_optimize(&mut self, kind: OptimizeKind, settings: &Settings, discard: &[Hidden]) {
        // What's typed in a form field is part of the document (#166).
        if !self.commit_form_typing() {
            return;
        }
        let Some((_, id)) = self.active_ids() else { return };
        if self.optimize_run.is_some() {
            self.notify_tr("Optimization is already running");
            return;
        }
        let job = match self.session.optimize_job(id, settings, discard) {
            Ok(job) => job,
            Err(e) => return self.save_optimized(id, kind, Err(e)),
        };
        let progress = Arc::new(Mutex::new(OptimizeProgress::default()));
        let p = progress.clone();
        let work = move || {
            let result = job.run(|stage| {
                let Ok(mut s) = p.lock() else { return false };
                s.stage = stage;
                !s.cancel
            });
            if let Ok(mut s) = p.lock() {
                s.result = Some(result);
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if self.run_inline {
            work();
        } else if let Err(e) = std::thread::Builder::new().name("pdfcraft-optimize".into()).spawn(work) {
            // No worker: report the failure instead of waiting for a result that never comes.
            if let Ok(mut s) = progress.lock() {
                s.result = Some(Err(EditError::Optimize(e.to_string())));
            }
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.optimize_run = Some(OptimizeRun { doc: id, kind, progress });
        self.poll_optimize();
    }

    /// Stop a running optimization (nothing is saved).
    pub fn cancel_optimize(&mut self) {
        if let Some(r) = &self.optimize_run
            && let Ok(mut s) = r.progress.lock()
        {
            s.cancel = true;
        }
    }

    /// Show progress; save the copy once the worker is done.
    pub(crate) fn poll_optimize(&mut self) {
        let Some(run) = self.optimize_run.as_ref() else { return };
        let (doc, kind, progress) = (run.doc, run.kind, run.progress.clone());
        let Ok(mut s) = progress.lock() else {
            // The worker panicked while holding the lock: it will never report back.
            self.optimize_run = None;
            self.progress_notice = None;
            self.notify_tr("Couldn't optimize the file");
            return;
        };
        let Some(result) = s.result.take() else {
            let cancelling = s.cancel;
            let stage = s.stage;
            drop(s);
            let label = if cancelling { tl!("Cancelling…").to_string() } else { stage_label(stage) };
            self.progress_notice = Some(crate::widgets::ProgressNotice { label, fraction: stage.fraction(), cancellable: !cancelling });
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            return;
        };
        drop(s);
        self.optimize_run = None;
        self.progress_notice = None;
        match result {
            Err(EditError::Cancelled) => self.notify_tr("Optimization cancelled"),
            result => {
                let result = result.map(|(b, r)| {
                    let detail = match kind {
                        OptimizeKind::Advanced => summary(&r),
                        OptimizeKind::Reduce(preset) => {
                            let before = self.session.get(doc).map_or(0, |d| d.bytes.len());
                            reduce_detail(&r, b.len(), before, preset)
                        }
                    };
                    (b, detail)
                });
                self.save_optimized(doc, kind, result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn the_reduce_notice_says_what_was_left_and_when_email_may_refuse_it() {
        crate::i18n::set_current(crate::i18n::Lang::EN);
        let mut r = OptimizeReport::default();
        let big = EMAIL_LIMIT * 2;
        assert_eq!(reduce_detail(&r, 1 << 20, big, ReducePreset::Balanced), "");
        r.optimize.images_unsupported = 1;
        assert_eq!(reduce_detail(&r, 1 << 20, big, ReducePreset::Balanced), ". 1 image was left as it is: PdfCraft can't recompress its format yet.");
        r.optimize.images_unsupported = 3;
        assert_eq!(
            reduce_detail(&r, EMAIL_LIMIT + 1, big, ReducePreset::Balanced),
            ". 3 images were left as they are: PdfCraft can't recompress their format yet. Still over 25 MB, the usual email limit. Try Smallest."
        );
        r.optimize.images_unsupported = 0;
        assert_eq!(reduce_detail(&r, EMAIL_LIMIT, big, ReducePreset::HighQuality), "", "25 MB itself is fine");
        assert_eq!(reduce_detail(&r, EMAIL_LIMIT + 1, big, ReducePreset::Smallest), ". Still over 25 MB, the usual email limit.");
        // A copy that isn't smaller isn't saved: the original, under the limit, is what's left.
        assert_eq!(reduce_detail(&r, EMAIL_LIMIT + 1, EMAIL_LIMIT - 1, ReducePreset::Balanced), "");
    }

    #[test]
    fn the_progress_card_shows_the_stage_and_cancels() {
        let progress = Arc::new(Mutex::new(OptimizeProgress { stage: OptimizeStage::Images { done: 3, total: 10 }, ..Default::default() }));
        let p = progress.clone();
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(move |_cc| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.open_bytes("notes.txt", None, b"hello".to_vec()).unwrap();
            let (_, doc) = app.active_ids().unwrap();
            app.optimize_run = Some(OptimizeRun { doc, kind: OptimizeKind::Advanced, progress: p });
            app.notify("Saved notes.pdf");
            app
        });
        // 1.5 s (kittest steps a quarter second): the bar has eased in, the toast still shows.
        h.run_steps(6);
        if let Ok(path) = std::env::var("PDFCRAFT_OPTIMIZE_PROGRESS_SHOT") {
            h.render().unwrap().save(path).unwrap();
        }
        h.get_by_label("Optimizing… image 4 of 10");
        let shown = h.state().progress_notice.clone().unwrap();
        assert!((shown.fraction - 0.275).abs() < 1e-4, "{shown:?}");
        assert!(shown.cancellable);

        h.get_by_label("Cancel").click();
        h.run_steps(2);
        assert!(progress.lock().unwrap().cancel, "the worker is asked to stop");
        h.get_by_label("Cancelling…");
        assert!(h.query_by_label("Cancel").is_none(), "Cancel is not offered twice");

        // The worker stops and reports back.
        progress.lock().unwrap().result = Some(Err(EditError::Cancelled));
        h.run_steps(2);
        assert!(h.state().optimize_run.is_none() && h.state().progress_notice.is_none());
        assert_eq!(h.state().toast.as_ref().map(|t| t.0.as_str()), Some("Optimization cancelled"));
    }

    #[test]
    fn a_second_run_is_refused_while_one_is_going() {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("notes.txt", None, b"hello".to_vec()).unwrap();
        let (_, doc) = app.active_ids().unwrap();
        app.optimize_run = Some(OptimizeRun { doc, kind: OptimizeKind::Reduce(ReducePreset::Balanced), progress: Arc::default() });
        app.optimize_with_draft();
        assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some("Optimization is already running"));
    }
}
