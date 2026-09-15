// OpenCell server — 公益版 Excel Web 的 Rust 引擎服务
// 架构：浏览器纯 HTML/DOM 渲染，本服务通过 HTTP/JSON 提供全部引擎能力
// （公式计算 IronCalc、xlsx 读写、样式、行列、多表、撤销重做、剪贴板）。
// 参照 zhidoc unidoc-server 模式：本地常驻服务，不使用 WASM。

mod ai_context;
mod ai_gateway;
mod cf_manager;
mod mcp;
mod native_chart_edit;
mod native_data_runtime;
mod native_page_review_edit;
mod native_pivot_cache_edit;
mod native_pivot_table_edit;
mod native_shape_edit;
mod native_slicer_edit;
mod native_smartart_edit;
mod native_table_edit;
mod native_timeline_edit;
mod pivot_local_refresh;
mod print_layout;
mod protection_runtime;
mod validation_runtime;
mod what_if_runtime;

use ironcalc::base::BorderArea;
use ironcalc::base::ClipboardData;
use ironcalc::base::UserModel;
use ironcalc::base::cell::CellValue;
use ironcalc::base::expressions::types::{Area, CellReferenceIndex};
use ironcalc::base::types::{
    Color, IterationSettings, RichTextRun, Style, Table, TableColumn, TableStyleInfo, Theme,
};
use ironcalc::export::save_to_xlsx;
use ironcalc::import::load_from_xlsx;
use serde_json::{Value, json};
use sha2::Digest;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// 8139 已被本机其他工具占用；UniCell 固定用 8143，可用 --port= 或 UNICELL_PORT 覆盖。
const DEFAULT_PORT: u16 = 8143;
const MAX_BODY: usize = 64 * 1024 * 1024; // 64 MiB（xlsx 上传上限）
const MAX_ROWS: i32 = 1_048_576;
const MAX_COLS: i32 = 16_384;
const HTML_CAPTURE_MAX_PIXELS: u64 = 32 * 1024 * 1024;
const MAX_OPC_UNCOMPRESSED: usize = 256 * 1024 * 1024;
const MAX_UDOC3_ENTRY_SIZE: usize = 512 * 1024 * 1024;
const MAX_UDOC3_TOTAL_SIZE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_UDOC3_ZIP_SIZE: usize = MAX_UDOC3_ENTRY_SIZE + 1024 * 1024;
const MAX_UDOC3_ENTRIES: usize = 100_000;
static HTML_CAPTURE_SEQ: AtomicU64 = AtomicU64::new(1);
static DATA_VALIDATION_SEQ: AtomicU64 = AtomicU64::new(1);
static CLIPBOARD_OBJECT_SEQ: AtomicU64 = AtomicU64::new(1);
static XLSX_TEMP_SEQ: AtomicU64 = AtomicU64::new(1);
static ACTIVE_AI_CHATS: AtomicUsize = AtomicUsize::new(0);
// Bound concurrent upstream requests in a local process.
const MAX_ACTIVE_AI_CHATS: usize = 12;

const SESSION_COOKIE_NAME: &str = "unicell_session";
const SESSION_ID_BYTES: usize = 32;
const SESSION_IDLE_TTL: std::time::Duration = std::time::Duration::from_secs(8 * 60 * 60);
const MAX_ACTIVE_SESSIONS: usize = 32;

fn unique_xlsx_temp_path(purpose: &str) -> PathBuf {
    let sequence = XLSX_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "unicell-{purpose}-{}-{sequence}.xlsx",
        std::process::id()
    ))
}

#[derive(Debug, Clone, Default)]
struct OpcPackageSnapshot {
    /// Original uncompressed OPC parts. Controlled spreadsheet parts are rebuilt by IronCalc;
    /// unsupported parts are copied back and reconnected during export.
    parts: std::collections::BTreeMap<String, Vec<u8>>,
    macro_enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct FormulaTransportSnapshot {
    sheets: std::collections::HashMap<String, FormulaTransportSheet>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct FormulaTransportSheet {
    sheet_index: u32,
    groups: Vec<FormulaTransportGroup>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct FormulaTransportGroup {
    cells: Vec<FormulaTransportCell>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct FormulaTransportCell {
    reference: String,
    baseline_content: String,
    raw_formula: String,
    cell_metadata: Option<String>,
    value_metadata: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct WorksheetFeatureTransport {
    /// IronCalc's semantic CF state immediately after import.  This lets export distinguish a
    /// truly untouched sheet (restore raw OOXML) from a user edit (perform rule-level merge).
    baseline_conditional_formatting: std::collections::HashMap<u32, String>,
    /// Standard worksheet data validations have no IronCalc model.  Keep a typed editable view
    /// beside the exact OOXML fragments so changing one rule does not flatten unknown attributes,
    /// extension children, or the untouched rules around it.
    data_validations: std::collections::HashMap<u32, DataValidationSheet>,
    data_validation_dirty: std::collections::HashSet<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct DataValidationSheet {
    container_start_tag: String,
    rules: Vec<DataValidationRule>,
}

fn serde_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataValidationRule {
    #[serde(default)]
    id: String,
    sqref: String,
    #[serde(rename = "type", default = "data_validation_any_type")]
    validation_type: String,
    #[serde(default)]
    operator: Option<String>,
    #[serde(default)]
    allow_blank: bool,
    /// OOXML's showDropDown flag is inverted: true means hide the in-cell arrow.
    #[serde(default = "serde_true")]
    in_cell_dropdown: bool,
    #[serde(default)]
    show_input_message: bool,
    #[serde(default)]
    show_error_message: bool,
    #[serde(default)]
    error_style: Option<String>,
    #[serde(default)]
    ime_mode: Option<String>,
    #[serde(default)]
    prompt_title: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    error_title: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    formula1: Option<String>,
    #[serde(default)]
    formula2: Option<String>,
    /// Exact imported rule.  Serialization patches only known fields into this fragment.
    #[serde(skip)]
    raw_xml: Option<String>,
}

fn data_validation_any_type() -> String {
    "any".to_string()
}

/// State that lives beside IronCalc's model but must participate in the same
/// user-visible undo/redo timeline.  The original OPC package is immutable for
/// the lifetime of an imported workbook, so the snapshot only stores editable
/// overlays and transport metadata.
#[derive(Debug, Clone, PartialEq)]
struct AppSidecarSnapshot {
    clip_tsv: Option<String>,
    clip_engine: Option<Value>,
    selected_sheet: u32,
    selected_range: [i32; 4],
    calculation_mode: CalculationMode,
    calculation_properties_dirty: bool,
    iteration_settings: IterationSettings,
    model_tables: std::collections::HashMap<String, Table>,
    cf_sheets: std::collections::HashSet<u32>,
    objects: std::collections::HashMap<u32, Vec<Value>>,
    rich_text: std::collections::HashMap<(u32, i32, i32), Vec<RichTextRun>>,
    rich_text_xml: std::collections::HashMap<(u32, i32, i32), String>,
    formula_transport: FormulaTransportSnapshot,
    worksheet_features: WorksheetFeatureTransport,
    pivot_cache_refresh_edits: std::collections::HashMap<String, Value>,
    native_pivot_table_edits: Vec<Value>,
    native_pivot_local_refresh_edits: Vec<Value>,
    native_slicer_edits: Vec<Value>,
    native_timeline_edits: Vec<Value>,
    native_data_edits: Vec<Value>,
    native_table_edits: Vec<Value>,
    native_page_review_edits: Vec<Value>,
    native_table_model: Option<Value>,
    what_if_scenarios: what_if_runtime::ScenarioStore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalculationMode {
    Automatic,
    Manual,
}

impl CalculationMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone)]
struct AppHistoryEntry {
    before: AppSidecarSnapshot,
    after: AppSidecarSnapshot,
    /// A request can produce several IronCalc history units (multi-cell external
    /// paste and replace-all are the common cases).  Replaying the exact count
    /// prevents an OOXML-only action from accidentally undoing an older cell edit.
    model_steps: usize,
}

const APP_HISTORY_LIMIT: usize = 100;

#[derive(Debug)]
struct AppState {
    model: UserModel<'static>,
    /// Opaque, non-authorizing namespace used only to partition browser-side
    /// autosave and recent-file records belonging to this server session.
    storage_scope: String,
    calculation_mode: CalculationMode,
    calculation_properties_dirty: bool,
    // 应用内复制：保存 TSV 文本以便与系统剪贴板比对，一致则走引擎剪贴板粘贴
    clip_tsv: Option<String>,
    clip_engine: Option<Value>, // 序列化的引擎 Clipboard{sheet,range,data}，保留公式/样式
    file_name: String,
    cf_sheets: std::collections::HashSet<u32>, // 含条件格式规则的 sheet（决定 view 轻量/精确样式路径）
    objects: std::collections::HashMap<u32, Vec<Value>>, // sheet -> 插入对象列表（文本框/图片/SVG/视频/沙盒HTML）
    // IronCalc keeps the calculation value as plain text; retain OOXML per-run formatting
    // separately so one cell can still render mixed bold/size/font/color faithfully.
    rich_text: std::collections::HashMap<(u32, i32, i32), Vec<RichTextRun>>,
    rich_text_xml: std::collections::HashMap<(u32, i32, i32), String>,
    source_ooxml: Option<OpcPackageSnapshot>,
    formula_transport: FormulaTransportSnapshot,
    worksheet_features: WorksheetFeatureTransport,
    /// Stable cache-part -> refresh-attribute patch. Pivot layout/cache records stay native.
    pivot_cache_refresh_edits: std::collections::HashMap<String, Value>,
    /// Ordered, typed OOXML edit journals.  They are replayed atomically over the imported
    /// package so cross-part names, relationships and DrawingML anchors remain native.
    native_pivot_table_edits: Vec<Value>,
    native_pivot_local_refresh_edits: Vec<Value>,
    native_slicer_edits: Vec<Value>,
    native_timeline_edits: Vec<Value>,
    native_data_edits: Vec<Value>,
    native_table_edits: Vec<Value>,
    native_page_review_edits: Vec<Value>,
    native_table_model: Option<Value>,
    /// Excel-style Scenario Manager definitions. They share the application transaction history
    /// with cell edits so create/update/delete/apply all obey Ctrl+Z/Ctrl+Y ordering.
    what_if_scenarios: what_if_runtime::ScenarioStore,
    app_undo: Vec<AppHistoryEntry>,
    app_redo: Vec<AppHistoryEntry>,
    excel_extension: String,
    excel_mime: String,
}

impl AppState {
    fn new() -> Self {
        Self::new_with_storage_scope(format!("local-{}", random_session_id()))
    }

    fn new_with_storage_scope(storage_scope: impl Into<String>) -> Self {
        let storage_scope = storage_scope.into();
        let model =
            UserModel::new_empty("Book1", "en", "UTC", "en").expect("cannot create empty model");
        AppState {
            model,
            storage_scope: storage_scope.clone(),
            calculation_mode: CalculationMode::Automatic,
            calculation_properties_dirty: false,
            clip_tsv: None,
            clip_engine: None,
            file_name: "工作簿1".to_string(),
            cf_sheets: Default::default(),
            objects: Default::default(),
            rich_text: Default::default(),
            rich_text_xml: Default::default(),
            source_ooxml: None,
            formula_transport: Default::default(),
            worksheet_features: Default::default(),
            pivot_cache_refresh_edits: Default::default(),
            native_pivot_table_edits: Default::default(),
            native_pivot_local_refresh_edits: Default::default(),
            native_slicer_edits: Default::default(),
            native_timeline_edits: Default::default(),
            native_data_edits: Default::default(),
            native_table_edits: Default::default(),
            native_page_review_edits: Default::default(),
            native_table_model: None,
            what_if_scenarios: Default::default(),
            app_undo: Default::default(),
            app_redo: Default::default(),
            excel_extension: "xlsx".to_string(),
            excel_mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                .to_string(),
        }
    }

    /// Builds the immutable visual baseline copied into every newly issued
    /// browser session. It contains no imported data, filesystem paths,
    /// recovery records, external links, macros, or user metadata.
    fn new_landing(storage_scope: impl Into<String>) -> Self {
        let mut state = Self::new_with_storage_scope(storage_scope);
        state.file_name = "UniCell 默认工作簿".to_string();

        let model = &mut state.model;
        let author = (|| -> Result<(), String> {
            model.rename_sheet(0, "欢迎使用")?;
            model.new_sheet()?;
            model.rename_sheet(1, "工作表1")?;
            model.set_selected_sheet(0)?;

            model.set_columns_width(0, 1, 6, 118.0)?;
            model.set_rows_height(0, 1, 2, 34.0)?;
            model.set_rows_height(0, 3, 3, 28.0)?;
            model.set_rows_height(0, 6, 6, 26.0)?;
            model.set_rows_height(0, 7, 9, 24.0)?;
            model.set_rows_height(0, 11, 11, 28.0)?;

            model.merge_cells_range(0, 1, 1, 2, 6)?;
            model.merge_cells_range(0, 3, 1, 3, 6)?;
            model.merge_cells_range(0, 6, 1, 6, 2)?;
            model.merge_cells_range(0, 6, 3, 6, 4)?;
            model.merge_cells_range(0, 6, 5, 6, 6)?;
            model.merge_cells_range(0, 7, 1, 9, 2)?;
            model.merge_cells_range(0, 7, 3, 9, 4)?;
            model.merge_cells_range(0, 7, 5, 9, 6)?;
            model.merge_cells_range(0, 11, 1, 11, 6)?;

            model.set_user_input(0, 1, 1, "UniCell")?;
            model.set_user_input(0, 3, 1, "一个安全、独立的电子表格工作区")?;
            model.set_user_input(0, 6, 1, "打开工作簿")?;
            model.set_user_input(0, 6, 3, "直接编辑")?;
            model.set_user_input(0, 6, 5, "安全隔离")?;
            model.set_user_input(
                0,
                7,
                1,
                "从“文件 → 打开”导入 Excel、CSV、UniDoc 或 HTML 文档。",
            )?;
            model.set_user_input(
                0,
                7,
                3,
                "切换到“工作表1”，即可像 Excel 一样输入数据和公式。",
            )?;
            model.set_user_input(
                0,
                7,
                5,
                "此工作簿仅属于当前浏览器会话，不会显示其他用户的数据。",
            )?;
            model.set_user_input(0, 11, 1, "提示：Ctrl+S 保存，Ctrl+P 打印，Ctrl+Z 撤销。")?;

            let header = area(0, 1, 1, 2, 6);
            for (path, value) in [
                ("fill.color", "#107C41"),
                ("font.color", "#FFFFFF"),
                ("font.b", "true"),
                ("font.size", "24"),
                ("alignment.horizontal", "center"),
                ("alignment.vertical", "center"),
            ] {
                model.update_range_style(&header, path, value)?;
            }

            let subtitle = area(0, 3, 1, 3, 6);
            for (path, value) in [
                ("fill.color", "#E9F5EE"),
                ("font.color", "#185C37"),
                ("font.size", "12"),
                ("alignment.horizontal", "center"),
                ("alignment.vertical", "center"),
            ] {
                model.update_range_style(&subtitle, path, value)?;
            }

            for range in [
                area(0, 6, 1, 6, 2),
                area(0, 6, 3, 6, 4),
                area(0, 6, 5, 6, 6),
            ] {
                for (path, value) in [
                    ("fill.color", "#DCEFE4"),
                    ("font.color", "#185C37"),
                    ("font.b", "true"),
                    ("font.size", "12"),
                    ("alignment.horizontal", "center"),
                    ("alignment.vertical", "center"),
                ] {
                    model.update_range_style(&range, path, value)?;
                }
            }

            for range in [
                area(0, 7, 1, 9, 2),
                area(0, 7, 3, 9, 4),
                area(0, 7, 5, 9, 6),
            ] {
                for (path, value) in [
                    ("fill.color", "#F7FBF8"),
                    ("font.color", "#374151"),
                    ("font.size", "11"),
                    ("alignment.horizontal", "center"),
                    ("alignment.vertical", "center"),
                    ("alignment.wrap_text", "true"),
                ] {
                    model.update_range_style(&range, path, value)?;
                }
            }

            let footer = area(0, 11, 1, 11, 6);
            for (path, value) in [
                ("font.color", "#6B7280"),
                ("font.size", "10"),
                ("alignment.horizontal", "center"),
                ("alignment.vertical", "center"),
            ] {
                model.update_range_style(&footer, path, value)?;
            }
            model.set_selected_sheet(0)?;
            model.set_selected_cell(1, 1)?;
            model.set_selected_range(1, 1, 2, 6)?;
            Ok(())
        })();
        author.expect("built-in UniCell landing workbook must be valid");

        state.model.discard_all_history();
        let _ = state.model.flush_send_queue();
        state.clear_application_history();
        state
    }

    fn sidecar_snapshot(&self) -> AppSidecarSnapshot {
        let selected = self.model.get_selected_view();
        AppSidecarSnapshot {
            clip_tsv: self.clip_tsv.clone(),
            clip_engine: self.clip_engine.clone(),
            selected_sheet: selected.sheet,
            selected_range: selected.range,
            calculation_mode: self.calculation_mode,
            calculation_properties_dirty: self.calculation_properties_dirty,
            iteration_settings: self.model.get_iteration_settings(),
            model_tables: self.model.get_tables(),
            cf_sheets: self.cf_sheets.clone(),
            objects: self.objects.clone(),
            rich_text: self.rich_text.clone(),
            rich_text_xml: self.rich_text_xml.clone(),
            formula_transport: self.formula_transport.clone(),
            worksheet_features: self.worksheet_features.clone(),
            pivot_cache_refresh_edits: self.pivot_cache_refresh_edits.clone(),
            native_pivot_table_edits: self.native_pivot_table_edits.clone(),
            native_pivot_local_refresh_edits: self.native_pivot_local_refresh_edits.clone(),
            native_slicer_edits: self.native_slicer_edits.clone(),
            native_timeline_edits: self.native_timeline_edits.clone(),
            native_data_edits: self.native_data_edits.clone(),
            native_table_edits: self.native_table_edits.clone(),
            native_page_review_edits: self.native_page_review_edits.clone(),
            native_table_model: self.native_table_model.clone(),
            what_if_scenarios: self.what_if_scenarios.clone(),
        }
    }

    fn restore_sidecars(&mut self, snapshot: &AppSidecarSnapshot) {
        self.clip_tsv = snapshot.clip_tsv.clone();
        self.clip_engine = snapshot.clip_engine.clone();
        self.calculation_mode = snapshot.calculation_mode;
        self.calculation_properties_dirty = snapshot.calculation_properties_dirty;
        self.model
            .set_iteration_settings(snapshot.iteration_settings.clone())
            .expect("history contains validated iteration settings");
        match self.calculation_mode {
            CalculationMode::Automatic => self.model.resume_evaluation(),
            CalculationMode::Manual => self.model.pause_evaluation(),
        }
        self.model.replace_tables(snapshot.model_tables.clone());
        self.cf_sheets = snapshot.cf_sheets.clone();
        self.objects = snapshot.objects.clone();
        self.rich_text = snapshot.rich_text.clone();
        self.rich_text_xml = snapshot.rich_text_xml.clone();
        self.formula_transport = snapshot.formula_transport.clone();
        self.worksheet_features = snapshot.worksheet_features.clone();
        self.pivot_cache_refresh_edits = snapshot.pivot_cache_refresh_edits.clone();
        self.native_pivot_table_edits = snapshot.native_pivot_table_edits.clone();
        self.native_pivot_local_refresh_edits = snapshot.native_pivot_local_refresh_edits.clone();
        self.native_slicer_edits = snapshot.native_slicer_edits.clone();
        self.native_timeline_edits = snapshot.native_timeline_edits.clone();
        self.native_data_edits = snapshot.native_data_edits.clone();
        self.native_table_edits = snapshot.native_table_edits.clone();
        self.native_page_review_edits = snapshot.native_page_review_edits.clone();
        self.native_table_model = snapshot.native_table_model.clone();
        self.what_if_scenarios = snapshot.what_if_scenarios.clone();
        let [r0, c0, r1, c1] = snapshot.selected_range;
        if self
            .model
            .set_selected_sheet(snapshot.selected_sheet)
            .is_ok()
        {
            let _ = self.model.set_selected_cell(r0, c0);
            let _ = self.model.set_selected_range(r0, c0, r1, c1);
        }
    }

    fn clear_application_history(&mut self) {
        self.app_undo.clear();
        self.app_redo.clear();
        self.model.discard_redo_history();
    }

    fn record_application_history(
        &mut self,
        before: AppSidecarSnapshot,
        model_depth_before: usize,
    ) {
        let after = self.sidecar_snapshot();
        let model_steps = self.model.undo_depth().saturating_sub(model_depth_before);
        if model_steps == 0 && before == after {
            return;
        }
        // A new branch invalidates redo even when this request only touched
        // native OOXML sidecars and therefore produced no IronCalc diff.
        self.model.discard_redo_history();
        self.app_redo.clear();
        self.app_undo.push(AppHistoryEntry {
            before,
            after,
            model_steps,
        });
        if self.app_undo.len() > APP_HISTORY_LIMIT {
            self.app_undo.remove(0);
        }
    }

    fn undo_application(&mut self) -> Result<bool, String> {
        let Some(entry) = self.app_undo.pop() else {
            return Ok(false);
        };
        let mut completed = 0usize;
        while completed < entry.model_steps {
            if let Err(error) = self.model.undo() {
                for _ in 0..completed {
                    let _ = self.model.redo();
                }
                self.app_undo.push(entry);
                return Err(error);
            }
            completed += 1;
        }
        self.restore_sidecars(&entry.before);
        self.app_redo.push(entry);
        Ok(true)
    }

    fn redo_application(&mut self) -> Result<bool, String> {
        let Some(entry) = self.app_redo.pop() else {
            return Ok(false);
        };
        let mut completed = 0usize;
        while completed < entry.model_steps {
            if let Err(error) = self.model.redo() {
                for _ in 0..completed {
                    let _ = self.model.undo();
                }
                self.app_redo.push(entry);
                return Err(error);
            }
            completed += 1;
        }
        self.restore_sidecars(&entry.after);
        self.app_undo.push(entry);
        Ok(true)
    }
}

#[derive(Debug)]
struct SessionEntry {
    state: AppState,
    last_seen: std::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionResolution {
    id: String,
    set_cookie: bool,
}

#[derive(Debug, Default)]
struct SessionStore {
    sessions: std::collections::HashMap<String, SessionEntry>,
}

impl SessionStore {
    fn make_room_for_session(&mut self) -> bool {
        if self.sessions.len() < MAX_ACTIVE_SESSIONS {
            return true;
        }
        // Capacity pressure may discard only an untouched landing workbook.
        let candidate = self
            .sessions
            .iter()
            .filter(|(_, entry)| {
                entry.state.file_name == "UniCell 默认工作簿"
                    && entry.state.app_undo.is_empty()
                    && entry.state.app_redo.is_empty()
                    && entry.state.source_ooxml.is_none()
                    && entry.state.objects.is_empty()
            })
            .min_by_key(|(_, entry)| entry.last_seen)
            .map(|(id, _)| id.clone());
        if let Some(id) = candidate {
            self.sessions.remove(&id);
        }
        self.sessions.len() < MAX_ACTIVE_SESSIONS
    }

    fn resolve(&mut self, presented: Option<&str>) -> Result<SessionResolution, String> {
        let now = std::time::Instant::now();
        self.sessions.retain(|_, entry| {
            now.checked_duration_since(entry.last_seen)
                .unwrap_or_default()
                <= SESSION_IDLE_TTL
        });

        if let Some(id) = presented.filter(|id| valid_session_id(id)) {
            if let Some(entry) = self.sessions.get_mut(id) {
                entry.last_seen = now;
                return Ok(SessionResolution {
                    id: id.to_string(),
                    set_cookie: false,
                });
            }
        }

        if !self.make_room_for_session() {
            return Err(
                "too many active workbook sessions; close an idle session and retry".into(),
            );
        }
        let id = loop {
            let candidate = random_session_id();
            if !self.sessions.contains_key(&candidate) {
                break candidate;
            }
        };
        let storage_scope = storage_scope_for_session(&id);
        self.sessions.insert(
            id.clone(),
            SessionEntry {
                state: AppState::new_landing(storage_scope),
                last_seen: now,
            },
        );
        Ok(SessionResolution {
            id,
            set_cookie: true,
        })
    }

    fn state_mut(&mut self, id: &str) -> Result<&mut AppState, String> {
        Ok(&mut self.entry_mut(id)?.state)
    }

    fn entry_mut(&mut self, id: &str) -> Result<&mut SessionEntry, String> {
        let entry = self
            .sessions
            .get_mut(id)
            .ok_or("workbook session expired; reload to start a clean session")?;
        entry.last_seen = std::time::Instant::now();
        Ok(entry)
    }
}

fn random_session_id() -> String {
    let mut bytes = [0u8; SESSION_ID_BYTES];
    getrandom::fill(&mut bytes).expect("operating-system randomness is required for sessions");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_session_id(value: &str) -> bool {
    value.len() == SESSION_ID_BYTES * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn storage_scope_for_session(session_id: &str) -> String {
    let digest = format!("{:x}", sha2::Sha256::digest(session_id.as_bytes()));
    format!("session-{}", &digest[..24])
}

fn parse_session_cookie_header(value: &str) -> Option<String> {
    let mut found = None;
    for part in value.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        if name.trim() != SESSION_COOKIE_NAME {
            continue;
        }
        let value = value.trim();
        if found.is_some() || !valid_session_id(value) {
            return None;
        }
        found = Some(value.to_string());
    }
    found
}

fn request_session_cookie(request: &tiny_http::Request) -> Option<String> {
    request.headers().iter().find_map(|header| {
        if header.field.equiv("Cookie") {
            parse_session_cookie_header(header.value.as_str())
        } else {
            None
        }
    })
}

fn body_operation_mutates(body: &[u8]) -> bool {
    if body.is_empty() {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        // Let the endpoint report malformed JSON; the history wrapper will
        // observe no state change when parsing fails.
        return true;
    };
    !matches!(
        value.get("op").and_then(Value::as_str),
        Some("list" | "get" | "inspect" | "preview" | "validate")
    )
}

fn request_resets_history(path: &str) -> bool {
    matches!(
        path,
        "/api/import" | "/api/import-csv" | "/api/import-html" | "/api/import-udoc" | "/api/new"
    )
}

fn ai_apply_request_mutates(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("dryRun").cloned())
        .map(|value| value.as_bool() == Some(false))
        .unwrap_or(false)
}

fn request_may_mutate(path: &str, body: &[u8]) -> bool {
    match path {
        // AI 写入默认 dry-run；只有显式确认 dryRun:false 才进入应用级历史事务。
        "/api/ai/apply" => ai_apply_request_mutates(body),
        "/api/rich-text" | "/api/objects" => !body.is_empty(),
        "/api/cf"
        | "/api/dv"
        | "/api/pivot-caches"
        | "/api/pivot-tables"
        | "/api/pivot-local-refresh"
        | "/api/tables"
        | "/api/native-data"
        | "/api/page-review"
        | "/api/what-if"
        | "/api/calcmode"
        | "/api/slicers"
        | "/api/timelines"
        | "/api/names" => body_operation_mutates(body),
        "/api/input" | "/api/inputrange" | "/api/batch" | "/api/style" | "/api/border"
        | "/api/merge" | "/api/fontname" | "/api/copystyle" | "/api/clear" | "/api/rows"
        | "/api/cols" | "/api/colwidth" | "/api/rowheight" | "/api/sheet" | "/api/autofill"
        | "/api/paste" | "/api/replace" | "/api/freeze" | "/api/sort" | "/api/filter" => true,
        _ => false,
    }
}

fn handle_api_with_history(
    st: &mut AppState,
    path: &str,
    query: &str,
    body: &[u8],
) -> Result<Resp, String> {
    if request_resets_history(path) {
        let result = handle_api(st, path, query, body);
        if result.is_ok() {
            st.clear_application_history();
        }
        return result;
    }
    if path == "/api/undo" || path == "/api/redo" || !request_may_mutate(path, body) {
        return handle_api(st, path, query, body);
    }

    let before = st.sidecar_snapshot();
    let model_depth_before = st.model.undo_depth();
    let result = handle_api(st, path, query, body);
    if result.is_ok() {
        st.record_application_history(before, model_depth_before);
    } else {
        // Endpoints are transaction boundaries: do not leave half-applied native
        // XML overlays or model diffs behind when validation fails part-way.
        let model_steps = st.model.undo_depth().saturating_sub(model_depth_before);
        for _ in 0..model_steps {
            let _ = st.model.undo();
        }
        st.restore_sidecars(&before);
    }
    result
}

/// `--port=N` wins over `UNICELL_PORT`, so a one-off run can move off a port that
/// another local tool already owns without touching the environment.
fn resolve_port() -> u16 {
    std::env::args()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--port=").map(str::to_string))
        .or_else(|| std::env::var("UNICELL_PORT").ok())
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|port| *port != 0)
        .unwrap_or(DEFAULT_PORT)
}

fn main() {
    let sessions = std::sync::RwLock::new(SessionStore::default());
    let web_root = find_web_root();
    let font_root = find_font_root(&web_root);
    let port = resolve_port();
    let server = tiny_http::Server::http(("127.0.0.1", port))
        .unwrap_or_else(|error| panic!("cannot bind 127.0.0.1:{port}: {error}"));
    println!("UniCell local edition: http://127.0.0.1:{port}/");
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let (sessions, web_root, font_root, server) =
                (&sessions, &web_root, &font_root, &server);
            scope.spawn(move || {
                for mut request in server.incoming_requests() {
                    let url = request.url().to_string();
                    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
                    // Local CLI clients may omit Origin, but must use a loopback Host.
                    if !request_header(&request, "Host")
                        .is_some_and(|host| allowed_request_origin(&format!("http://{host}"), port))
                        || !request_is_local_same_origin(&request, port)
                    {
                        let _ = request.respond(finalize_response(
                            json_bytes_response(
                                br#"{"ok":false,"error":"Local same-origin requests required"}"#
                                    .to_vec(),
                                403,
                            ),
                            true,
                            None,
                        ));
                        continue;
                    }
                    if path == "/api/ai/chat" {
                        if !ai_chat_request_allowed(&request, port) {
                            let _ = request.respond(json_bytes_response(
                                br#"{"ok":false,"error":"POST application/json required"}"#
                                    .to_vec(),
                                405,
                            ));
                            continue;
                        }
                        let mut body = Vec::new();
                        if request
                            .as_reader()
                            .take((ai_gateway::MAX_REQUEST_BYTES + 1) as u64)
                            .read_to_end(&mut body)
                            .is_err()
                            || body.len() > ai_gateway::MAX_REQUEST_BYTES
                        {
                            let _ = request.respond(json_bytes_response(
                                br#"{"ok":false,"error":"Request too large"}"#.to_vec(),
                                413,
                            ));
                            continue;
                        }
                        if ACTIVE_AI_CHATS.fetch_add(1, Ordering::AcqRel) >= MAX_ACTIVE_AI_CHATS {
                            ACTIVE_AI_CHATS.fetch_sub(1, Ordering::AcqRel);
                            let _ = request.respond(json_bytes_response(
                                br#"{"ok":false,"error":"AI busy","code":"AI_BUSY"}"#.to_vec(),
                                429,
                            ));
                            continue;
                        }
                        let spawned = std::thread::Builder::new()
                            .name("unicell-ai-chat".into())
                            .spawn(move || {
                                respond_ai_chat(request, body);
                                ACTIVE_AI_CHATS.fetch_sub(1, Ordering::AcqRel);
                            });
                        if spawned.is_err() {
                            ACTIVE_AI_CHATS.fetch_sub(1, Ordering::AcqRel);
                        }
                        continue;
                    }
                    let is_mcp = matches!(path, "/mcp" | "/mcp/");
                    let is_api = path.starts_with("/api/");
                    let private = is_api || is_mcp || matches!(path, "/" | "/index.html");
                    let mut session = None;
                    let response = (|| -> Result<Resp, String> {
                        if is_mcp && request.method() != &tiny_http::Method::Post {
                            return status_json(json!({"error":"POST required"}), 405);
                        }
                        if !matches!(
                            request.method(),
                            tiny_http::Method::Get
                                | tiny_http::Method::Head
                                | tiny_http::Method::Post
                        ) {
                            return status_json(json!({"error":"Unsupported method"}), 405);
                        }
                        if private {
                            session = Some(
                                sessions
                                    .write()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .resolve(request_session_cookie(&request).as_deref())?,
                            );
                        }
                        if path == "/api/session" {
                            return ok_json(json!({"mode":"local","storage":"memory"}));
                        }
                        if is_api || is_mcp {
                            let mut body = Vec::new();
                            request
                                .as_reader()
                                .take((MAX_BODY + 1) as u64)
                                .read_to_end(&mut body)
                                .map_err(|_| "Cannot read request body")?;
                            if body.len() > MAX_BODY {
                                return status_json(json!({"error":"Request too large"}), 413);
                            }
                            if path == "/api/render-html-png" {
                                return api_render_html_png(&body);
                            }
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    let mut store =
                                        sessions.write().unwrap_or_else(|e| e.into_inner());
                                    let st = store
                                        .state_mut(&session.as_ref().expect("local session").id)?;
                                    if is_mcp {
                                        mcp::handle(st, &body)
                                    } else {
                                        handle_api_with_history(st, path, query, &body)
                                    }
                                }));
                            return result
                                .unwrap_or_else(|_| Err("Internal error (panic caught)".into()));
                        }
                        if matches!(path, "/mcp-protocol.html" | "/docs/mcp-protocol.html") {
                            return Ok(mcp::protocol_document());
                        }
                        if let Some(value) = mcp::metadata(path) {
                            return ok_json(value);
                        }
                        serve_static(web_root, font_root.as_ref(), path, query)
                    })();
                    let cookie = session
                        .as_ref()
                        .filter(|s| s.set_cookie)
                        .map(|s| s.id.as_str());
                    let response = response.unwrap_or_else(|message| {
                        json_bytes_response(
                            json!({"ok":false,"error":message}).to_string().into_bytes(),
                            400,
                        )
                    });
                    let _ = request.respond(finalize_response(response, private, cookie));
                }
            });
        }
    });
}

fn request_header(request: &tiny_http::Request, name: &'static str) -> Option<String> {
    request.headers().iter().find_map(|header| {
        header
            .field
            .equiv(name)
            .then(|| header.value.as_str().to_string())
    })
}

fn ai_chat_request_allowed(request: &tiny_http::Request, port: u16) -> bool {
    if request.method() != &tiny_http::Method::Post {
        return false;
    }
    if !request_header(request, "Content-Type")
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"))
    {
        return false;
    }
    if request_header(request, "Sec-Fetch-Site")
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
    {
        return false;
    }
    request_header(request, "Origin").is_none_or(|origin| allowed_request_origin(&origin, port))
}

fn allowed_request_origin(origin: &str, port: u16) -> bool {
    origin == format!("http://127.0.0.1:{port}")
        || origin == format!("http://localhost:{port}")
        || origin == format!("http://[::1]:{port}")
}

fn request_is_local_same_origin(request: &tiny_http::Request, port: u16) -> bool {
    if request_header(request, "Sec-Fetch-Site")
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
    {
        return false;
    }
    request_header(request, "Origin").is_none_or(|origin| allowed_request_origin(&origin, port))
}

fn respond_ai_chat(mut request: tiny_http::Request, body: Vec<u8>) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ai_gateway::chat(&body)))
        .unwrap_or_else(|_| Err("AI 网关内部错误".into()));
    let response = match result {
        Ok(value) => ok_json(value).expect("AI success response is JSON"),
        Err(error) => {
            let (status, code) = if error.starts_with("AI_NOT_CONFIGURED") {
                (503, "AI_NOT_CONFIGURED")
            } else if error.starts_with("AI 上游")
                || error.starts_with("无法创建 AI")
                || error.starts_with("无法读取 AI 上游")
            {
                (502, "AI_UPSTREAM_ERROR")
            } else {
                (400, "AI_REQUEST_INVALID")
            };
            status_json(json!({ "error": error, "code": code }), status)
                .expect("AI error response is JSON")
        }
    };
    let _ = request.respond(finalize_response(response, true, None));
}

type Resp = tiny_http::Response<std::io::Cursor<Vec<u8>>>;

fn json_bytes_response(bytes: Vec<u8>, status: u16) -> Resp {
    tiny_http::Response::from_data(bytes)
        .with_status_code(status)
        .with_header(
            tiny_http::Header::from_bytes(
                &b"Content-Type"[..],
                &b"application/json; charset=utf-8"[..],
            )
            .unwrap(),
        )
}

fn finalize_response(mut response: Resp, private: bool, session_cookie: Option<&str>) -> Resp {
    if private {
        for (name, value) in [
            ("Cache-Control", "private, no-store"),
            ("Pragma", "no-cache"),
            ("Vary", "Cookie"),
            ("Referrer-Policy", "no-referrer"),
        ] {
            response = response.with_header(
                tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap(),
            );
        }
    }
    if let Some(session_id) = session_cookie {
        let value =
            format!("{SESSION_COOKIE_NAME}={session_id}; Path=/; HttpOnly; SameSite=Strict");
        response = response.with_header(
            tiny_http::Header::from_bytes(&b"Set-Cookie"[..], value.as_bytes()).unwrap(),
        );
    }
    response
}

fn ok_json(v: Value) -> Result<Resp, String> {
    let mut obj = v;
    if obj.get("ok").is_none() {
        if let Some(map) = obj.as_object_mut() {
            map.insert("ok".into(), json!(true));
        }
    }
    Ok(json_bytes_response(obj.to_string().into_bytes(), 200))
}

fn status_json(mut value: Value, status: u16) -> Result<Resp, String> {
    if value.get("ok").is_none() {
        if let Some(map) = value.as_object_mut() {
            map.insert("ok".into(), json!(status < 400));
        }
    }
    Ok(json_bytes_response(value.to_string().into_bytes(), status))
}

fn find_web_root() -> PathBuf {
    let candidates = ["web", "../web", "../../web"];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.join("index.html").exists() {
            return p.canonicalize().unwrap_or(p);
        }
    }
    PathBuf::from("web")
}

fn find_font_root(web_root: &std::path::Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(value) = std::env::var("UNICELL_FONT_ROOT") {
        let value = value.trim();
        if !value.is_empty() {
            candidates.push(PathBuf::from(value));
        }
    }
    candidates.push(web_root.join("fonts"));
    candidates.into_iter().find_map(|candidate| {
        let root = candidate.canonicalize().ok()?;
        (root.is_dir() && root.join("manifest.json").is_file()).then_some(root)
    })
}

fn static_content_type(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "ttf" => "font/ttf",
        "ttc" | "otc" => "font/collection",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn valid_relative_asset_path(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|character| matches!(character, '\\' | '\0' | ':'))
        && std::path::Path::new(value)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn verified_font_cache_control(query: &str, path: &std::path::Path, bytes: &[u8]) -> &'static str {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("manifest.json"))
    {
        return "no-cache, must-revalidate";
    }
    let expected = qget(query, "v").unwrap_or_default();
    if expected.len() == 64
        && expected
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && format!("{:x}", sha2::Sha256::digest(bytes)) == expected
    {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache, must-revalidate"
    }
}

fn serve_static(
    root: &PathBuf,
    font_root: Option<&PathBuf>,
    path: &str,
    query: &str,
) -> Result<Resp, String> {
    let rel = if path == "/" {
        "index.html"
    } else {
        path.trim_start_matches('/')
    };
    // 阻止路径穿越
    if !valid_relative_asset_path(rel) {
        return Err("bad path".into());
    }
    let (file, is_font_request) = if let Some(encoded) = rel.strip_prefix("fonts/") {
        let decoded = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .map_err(|_| "bad font path encoding".to_string())?;
        if !valid_relative_asset_path(&decoded) {
            return Err("bad font path".into());
        }
        let font_root = font_root.ok_or("font repository is not configured")?;
        let file = font_root.join(decoded.as_ref());
        let canonical = file
            .canonicalize()
            .map_err(|_| format!("not found: {rel}"))?;
        if !canonical.starts_with(font_root) {
            return Err("bad font path".into());
        }
        (canonical, true)
    } else {
        let file = match root.join(rel).canonicalize() {
            Ok(file) => file,
            Err(_) => return status_json(json!({"error":"Not found"}), 404),
        };
        let canonical_root = root.canonicalize().map_err(|_| "Web root unavailable")?;
        if !file.starts_with(canonical_root) || !file.is_file() {
            return status_json(json!({"error":"Not found"}), 404);
        }
        (file, false)
    };
    let bytes = std::fs::read(&file).map_err(|_| format!("not found: {rel}"))?;
    let mime = static_content_type(&file);
    let mut response = tiny_http::Response::from_data(bytes.clone())
        .with_status_code(200)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], mime.as_bytes()).unwrap())
        .with_header(
            tiny_http::Header::from_bytes(&b"X-Content-Type-Options"[..], &b"nosniff"[..]).unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Permissions-Policy"[..], &b"local-fonts=(self)"[..])
                .unwrap(),
        );
    if is_font_request {
        response = response
            .with_header(
                tiny_http::Header::from_bytes(
                    &b"Cross-Origin-Resource-Policy"[..],
                    &b"same-origin"[..],
                )
                .unwrap(),
            )
            .with_header(
                tiny_http::Header::from_bytes(
                    &b"Cache-Control"[..],
                    verified_font_cache_control(query, &file, &bytes).as_bytes(),
                )
                .unwrap(),
            );
    }
    Ok(response)
}

// ---------- 查询参数与 JSON 工具 ----------

fn qget<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        if k == key { Some(v) } else { None }
    })
}

fn qi(query: &str, key: &str, default: i32) -> i32 {
    qget(query, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn ji(v: &Value, key: &str) -> Result<i64, String> {
    v.get(key)
        .and_then(|x| x.as_i64())
        .ok_or_else(|| format!("missing int field: {key}"))
}

fn js<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("missing str field: {key}"))
}

fn parse_body(body: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(body).map_err(|e| format!("bad json: {e}"))
}

fn find_chromium_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("UNICELL_CHROMIUM") {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

fn html_capture_geometry(body: &Value) -> Result<(u32, u32, u32), String> {
    let width = body["width"].as_u64().unwrap_or(240).clamp(1, 4096) as u32;
    let height = body["height"].as_u64().unwrap_or(140).clamp(1, 4096) as u32;
    let mut scale = body["scale"].as_u64().unwrap_or(3).clamp(1, 4) as u32;
    let base_pixels = u64::from(width) * u64::from(height);
    if base_pixels > HTML_CAPTURE_MAX_PIXELS {
        return Err("HTML capture area is too large".into());
    }
    while base_pixels * u64::from(scale) * u64::from(scale) > HTML_CAPTURE_MAX_PIXELS {
        scale -= 1;
    }
    Ok((width, height, scale.max(1)))
}

fn html_capture_document(html: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="color-scheme" content="light"><style>html,body{{width:100%;height:100%;margin:0;overflow:hidden;background:#fff}}#capture{{position:fixed;inset:0;display:block;width:100%;height:100%;border:0;background:#fff}}</style></head><body><iframe id="capture" sandbox="allow-scripts allow-forms allow-modals allow-popups" srcdoc="{}"></iframe></body></html>"#,
        html_escape(html)
    )
}

fn remove_capture_dir(path: &std::path::Path) {
    // path is always the exact per-request directory created below, never a caller-provided path.
    let _ = std::fs::remove_dir_all(path);
}

fn chromium_html_capture_once(
    chromium: &std::path::Path,
    source_url: &str,
    capture_dir: &std::path::Path,
    width: u32,
    height: u32,
    scale: u32,
    attempt: u8,
) -> Result<Vec<u8>, String> {
    // A fresh profile is required for each attempt. Reusing the profile left by
    // a process that exited with STATUS_BREAKPOINT (0x80000003) can make the
    // recovery attempt fail on Chromium's SingletonLock/crash-reporter state.
    let png_path = capture_dir.join(format!("capture-{attempt}.png"));
    let profile_path = capture_dir.join(format!("profile-{attempt}"));
    let mut child = std::process::Command::new(chromium)
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--disable-extensions")
        .arg("--disable-background-networking")
        .arg("--disable-breakpad")
        .arg("--disable-crash-reporter")
        .arg("--disable-component-update")
        .arg("--disable-default-apps")
        .arg("--disable-dev-shm-usage")
        .arg("--hide-scrollbars")
        .arg("--mute-audio")
        .arg("--no-default-browser-check")
        .arg("--no-first-run")
        .arg("--run-all-compositor-stages-before-draw")
        .arg("--default-background-color=ffffffff")
        .arg(format!("--user-data-dir={}", profile_path.display()))
        .arg(format!("--force-device-scale-factor={scale}"))
        .arg(format!("--window-size={width},{height}"))
        .arg("--virtual-time-budget=2200")
        .arg(format!("--screenshot={}", png_path.display()))
        .arg(source_url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("start Chromium capture: {e}"))?;

    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < std::time::Duration::from_secs(12) => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Chromium HTML capture timed out after 12 seconds".into());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for Chromium capture: {e}"));
            }
        }
    };
    if !status.success() {
        return Err(format!("Chromium capture exited with {status}"));
    }

    // Some Chromium builds terminate immediately before the screenshot file
    // becomes observable to this process. Keep the settling window bounded.
    let file_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !png_path.is_file() && std::time::Instant::now() < file_deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    match std::fs::read(&png_path) {
        Ok(bytes) if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Ok(bytes),
        Ok(_) => Err("Chromium capture did not produce a PNG".into()),
        Err(e) => Err(format!("read Chromium capture: {e}")),
    }
}

fn chromium_capture_error_is_retryable(error: &str) -> bool {
    // Invalid request geometry/source errors are checked before Chromium is
    // started. Only process-start/exit/file-observation failures are transient.
    !error.contains("timed out")
        && (error.starts_with("start Chromium capture:")
            || error.starts_with("wait for Chromium capture:")
            || error.starts_with("Chromium capture exited with")
            || error.starts_with("Chromium capture did not produce")
            || error.starts_with("read Chromium capture:"))
}

fn api_render_html_png(body: &[u8]) -> Result<Resp, String> {
    let request = parse_body(body)?;
    let html = request["html"]
        .as_str()
        .ok_or("missing HTML capture source")?;
    if html.len() > 16 * 1024 * 1024 {
        return Err("HTML capture source exceeds 16 MiB".into());
    }
    let (width, height, scale) = html_capture_geometry(&request)?;
    let chromium = find_chromium_binary().ok_or("Chromium was not found; set UNICELL_CHROMIUM")?;
    let seq = HTML_CAPTURE_SEQ.fetch_add(1, Ordering::Relaxed);
    let capture_dir =
        std::env::temp_dir().join(format!("unicell-html-capture-{}-{seq}", std::process::id()));
    std::fs::create_dir(&capture_dir).map_err(|e| format!("create capture directory: {e}"))?;
    let source_path = capture_dir.join("capture.html");
    if let Err(e) = std::fs::write(&source_path, html_capture_document(html)) {
        remove_capture_dir(&capture_dir);
        return Err(format!("write capture document: {e}"));
    }
    let source_url = format!(
        "file:///{}",
        source_path.to_string_lossy().replace('\\', "/")
    );
    let mut last_error = String::new();
    let mut png = None;
    for attempt in 0..2u8 {
        match chromium_html_capture_once(
            &chromium,
            &source_url,
            &capture_dir,
            width,
            height,
            scale,
            attempt,
        ) {
            Ok(bytes) => {
                png = Some(bytes);
                break;
            }
            Err(error) => {
                let retry = attempt == 0 && chromium_capture_error_is_retryable(&error);
                last_error = error;
                if !retry {
                    break;
                }
                // STATUS_BREAKPOINT exits immediately; a short pause lets Windows
                // release crash-reporter/process resources without blocking the UI.
                std::thread::sleep(std::time::Duration::from_millis(125));
            }
        }
    }
    let Some(png) = png else {
        remove_capture_dir(&capture_dir);
        return Err(if last_error.is_empty() {
            "Chromium capture failed".into()
        } else if last_error.contains("timed out") {
            // Preserve the timeout prefix so the frontend does not start a
            // second HTTP capture after the backend already consumed its budget.
            last_error
        } else {
            format!("Chromium capture failed after bounded recovery: {last_error}")
        });
    };
    remove_capture_dir(&capture_dir);
    Ok(tiny_http::Response::from_data(png)
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"image/png"[..]).unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(
                &b"X-UniCell-Capture-Scale"[..],
                scale.to_string().as_bytes(),
            )
            .unwrap(),
        ))
}

fn clamp_range(r0: i32, c0: i32, r1: i32, c1: i32) -> (i32, i32, i32, i32) {
    let r0 = r0.clamp(1, MAX_ROWS);
    let r1 = r1.clamp(r0, MAX_ROWS);
    let c0 = c0.clamp(1, MAX_COLS);
    let c1 = c1.clamp(c0, MAX_COLS);
    (r0, c0, r1, c1)
}

fn area(sheet: u32, r0: i32, c0: i32, r1: i32, c1: i32) -> Area {
    let (r0, c0, r1, c1) = clamp_range(r0, c0, r1, c1);
    Area {
        sheet,
        row: r0,
        column: c0,
        width: c1 - c0 + 1,
        height: r1 - r0 + 1,
    }
}

// ---------- 样式序列化（发给前端渲染的紧凑格式） ----------

fn style_to_json(st: &AppState, s: &Style) -> Value {
    let font_color = st.model.resolve_color(&s.font.color);
    let fill_color = st.model.resolve_color(&s.fill.color);
    let (h, v, wrap) = match &s.alignment {
        Some(a) => (
            format!("{:?}", a.horizontal).to_lowercase(),
            format!("{:?}", a.vertical).to_lowercase(),
            a.wrap_text,
        ),
        None => ("general".to_string(), "bottom".to_string(), false),
    };
    // 边框：每边 {s:样式, c:颜色}，无边框省略
    let bi = |item: &Option<ironcalc::base::types::BorderItem>| -> Value {
        match item {
            Some(it) => json!({
                "s": format!("{:?}", it.style).to_lowercase(),
                "c": st.model.resolve_color(&it.color),
            }),
            None => Value::Null,
        }
    };
    let border = json!({
        "t": bi(&s.border.top),
        "b": bi(&s.border.bottom),
        "l": bi(&s.border.left),
        "r": bi(&s.border.right),
    });
    json!({
        "b": s.font.b,
        "i": s.font.i,
        "u": s.font.u,
        "st": s.font.strike,
        "sz": s.font.sz,
        "fn": s.font.name,
        "fc": font_color,
        "bg": fill_color,
        "ha": h,
        "va": v,
        "wr": wrap,
        "nf": s.num_fmt,
        "br": border,
    })
}

// ---------- API 调度 ----------

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn state_may_have_runtime_protection(st: &AppState) -> bool {
    st.native_page_review_edits.iter().any(|edit| {
        let encoded = edit.to_string();
        encoded.contains("sheetProtection") || encoded.contains("workbookProtection")
    }) || st.source_ooxml.as_ref().is_some_and(|snapshot| {
        snapshot.parts.iter().any(|(name, bytes)| {
            (name == "xl/workbook.xml"
                || (name.starts_with("xl/worksheets/") && name.ends_with(".xml")))
                && (bytes_contain(bytes, b"sheetProtection")
                    || bytes_contain(bytes, b"workbookProtection"))
        })
    })
}

fn enforce_runtime_protection(st: &AppState, path: &str, body: &[u8]) -> Result<(), String> {
    if !request_may_mutate(path, body) || !state_may_have_runtime_protection(st) {
        return Ok(());
    }
    let request = if body.is_empty() {
        json!({})
    } else {
        parse_body(body)?
    };
    let parts = materialize_page_review_parts(st)?;
    let model = native_page_review_edit::inspect_page_review_model(&parts)?;
    if path == "/api/ai/apply" {
        return enforce_ai_typed_ops_protection(st, &model, &parts, &request);
    }
    protection_runtime::enforce_mutation_with_parts(&model, &parts, path, &request)
}

fn enforce_ai_typed_ops_protection(
    st: &AppState,
    protection_model: &Value,
    parts: &std::collections::BTreeMap<String, Vec<u8>>,
    request: &Value,
) -> Result<(), String> {
    let items = request
        .get("ops")
        .and_then(Value::as_array)
        .ok_or("缺少 ops 数组")?;
    let names = st.model.get_model().workbook.get_worksheet_names();
    let default_sheet = ai_sheet_index(
        &names,
        request.get("sheet"),
        st.model.get_selected_view().sheet,
    )?;
    let ops = ai_parse_typed_ops(st, items, default_sheet, &names)?;
    let password = request.get("protectionPassword").cloned();
    let with_password = |mut body: Value| {
        if let Some(password) = &password {
            body["protectionPassword"] = password.clone();
        }
        body
    };
    let enforce = |path: &str, body: Value| {
        protection_runtime::enforce_mutation_with_parts(
            protection_model,
            parts,
            path,
            &with_password(body),
        )
    };

    // Check every exact cell, not only a bounding rectangle, so a sparse AI batch can write
    // multiple unlocked input cells without being rejected because a locked cell lies between.
    let mut cells_by_sheet = std::collections::BTreeMap::<u32, Vec<Value>>::new();
    for op in &ops {
        match op {
            AiTypedOp::Cells(items) => {
                for item in items {
                    let (sheet, row, col) = item.target();
                    cells_by_sheet
                        .entry(sheet)
                        .or_default()
                        .push(json!({"r":row,"c":col}));
                }
            }
            AiTypedOp::SetFormat { sheet, range, .. } => enforce(
                "/api/style",
                json!({"sheet":sheet,"r0":range[0],"c0":range[1],"r1":range[2],"c1":range[3]}),
            )?,
            AiTypedOp::SetBorder { sheet, range, .. } => enforce(
                "/api/border",
                json!({"sheet":sheet,"r0":range[0],"c0":range[1],"r1":range[2],"c1":range[3]}),
            )?,
            AiTypedOp::UpdateChart { sheet, .. } => {
                enforce("/api/objects", json!({"sheet":sheet}))?
            }
            AiTypedOp::UpdatePivotTable { part, .. } => {
                let pivot_sheet = pivot_table_model_with_edits(st)
                    .ok()
                    .and_then(|model| model.get("tables").and_then(Value::as_array).cloned())
                    .into_iter()
                    .flatten()
                    .find(|table| table.get("part").and_then(Value::as_str) == Some(part.as_str()))
                    .and_then(|table| {
                        table
                            .get("sheet")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .and_then(|sheet| names.iter().position(|name| name == &sheet))
                    .map(|sheet| sheet as u32);
                let body = pivot_sheet
                    .map(|sheet| json!({"sheet":sheet}))
                    .unwrap_or_else(|| json!({}));
                enforce("/api/pivot-tables", body)?;
            }
        }
    }
    for (sheet, cells) in cells_by_sheet {
        enforce("/api/batch", json!({"sheet":sheet,"cells":cells}))?;
    }
    Ok(())
}

fn handle_api(st: &mut AppState, path: &str, query: &str, body: &[u8]) -> Result<Resp, String> {
    enforce_runtime_protection(st, path, body)?;
    match path {
        "/api/info" => api_info(st),
        "/api/view" => api_view(st, query),
        "/api/cell" => api_cell(st, query),
        "/api/rich-text" => api_rich_text(st, query, body),
        "/api/input" => api_input(st, body),
        "/api/inputrange" => api_inputrange(st, body),
        "/api/batch" => api_batch(st, body),
        "/api/style" => api_style(st, body),
        "/api/border" => api_border(st, body),
        "/api/merge" => api_merge(st, body),
        "/api/objects" => api_objects(st, query, body),
        "/api/native-drawing/validate" => api_native_drawing_validate(st, body),
        "/api/cf" => api_cf(st, body),
        "/api/dv" => api_data_validation(st, body),
        "/api/pivot-caches" => api_pivot_caches(st, body),
        "/api/pivot-tables" => api_pivot_tables(st, body),
        "/api/pivot-local-refresh" => api_pivot_local_refresh(st, body),
        "/api/tables" => api_tables(st, body),
        "/api/native-data" => api_native_data(st, body),
        "/api/power-query/execute" => api_power_query_execute(body),
        "/api/page-review" => api_page_review(st, body),
        "/api/what-if" => api_what_if(st, body),
        "/api/sort" => api_sort_range(st, body),
        "/api/filter" => api_filter_range(st, body),
        "/api/slicers" => api_slicers(st, body),
        "/api/timelines" => api_timelines(st, body),
        "/api/fontname" => api_fontname(st, body),
        "/api/copystyle" => api_copystyle(st, body),
        "/api/clear" => api_clear(st, body),
        "/api/rows" => api_rows(st, body),
        "/api/cols" => api_cols(st, body),
        "/api/colwidth" => api_colwidth(st, body),
        "/api/rowheight" => api_rowheight(st, body),
        "/api/undo" => {
            let changed = st.undo_application()?;
            ok_json(json!({ "changed": changed }))
        }
        "/api/redo" => {
            let changed = st.redo_application()?;
            ok_json(json!({ "changed": changed }))
        }
        "/api/sheet" => api_sheet(st, body),
        "/api/autofill" => api_autofill(st, body),
        "/api/copy" => api_copy(st, body),
        "/api/paste" => api_paste(st, body),
        "/api/edge" => api_edge(st, query),
        "/api/stats" => api_stats(st, query),
        "/api/dimension" => api_dimension(st, query),
        "/api/find" => api_find(st, body),
        "/api/replace" => api_replace(st, body),
        "/api/calcmode" => api_calcmode(st, body),
        "/api/calc" => {
            st.model.evaluate();
            ok_json(json!({}))
        }
        "/api/names" => api_names(st, body),
        "/api/dependents" => api_dependents(st, query),
        "/api/freeze" => api_freeze(st, body),
        "/api/import" => api_import(st, body),
        "/api/import-csv" => api_import_csv(st, body),
        "/api/export" => api_export(st, query, body),
        "/api/export-csv" => api_export_csv(st, query),
        "/api/export-udoc" => api_export_udoc(st, query),
        "/api/export-html" => api_export_html(st, query),
        "/api/print-html" => api_print_html_paged(st, query),
        "/api/import-udoc" => api_import_udoc(st, body),
        "/api/import-html" => api_import_html(st, body),
        "/api/udoc-json" => api_udoc_json(st),
        "/api/ai/config" => ok_json(ai_gateway::public_status()),
        "/api/ai/chat" => ok_json(ai_gateway::chat(body)?),
        "/api/ai/context" => api_ai_context(st, body),
        "/api/ai/apply" => api_ai_apply(st, body),
        "/api/svg2emf" => api_svg2emf(st, body),
        "/api/emf2svg" => api_emf2svg(st, body),
        "/api/new" => {
            let storage_scope = st.storage_scope.clone();
            *st = AppState::new_with_storage_scope(storage_scope);
            ok_json(json!({}))
        }
        _ => status_json(json!({"error":"Unknown API"}), 404),
    }
}

fn api_info(st: &AppState) -> Result<Resp, String> {
    let names = st.model.get_model().workbook.get_worksheet_names();
    ok_json(json!({
        "sheets": names,
        "fileName": st.file_name,
        "storageScope": st.storage_scope,
        "sessionIsolated": true,
        "excelExtension": st.excel_extension,
        "excelMime": st.excel_mime,
        "canUndo": !st.app_undo.is_empty(),
        "canRedo": !st.app_redo.is_empty(),
        "maxRows": MAX_ROWS,
        "maxCols": MAX_COLS,
    }))
}

fn api_view(st: &AppState, query: &str) -> Result<Resp, String> {
    let sheet = qi(query, "sheet", 0) as u32;
    let (r0, c0, r1, c1) = clamp_range(
        qi(query, "r0", 1),
        qi(query, "c0", 1),
        qi(query, "r1", 50),
        qi(query, "c1", 26),
    );
    if (r1 - r0) > 500 || (c1 - c0) > 200 {
        return Err("viewport too large".into());
    }
    let mut cells = Vec::new();
    // 性能优化：无条件格式的表走轻量样式路径（免邻格边框查询）；
    // 空单元格无样式直接跳过，避免每格 4+ 次引擎调用
    let has_cf = st
        .model
        .get_conditional_formatting_list(sheet)
        .map(|rules| !rules.is_empty())
        .unwrap_or_else(|_| st.cf_sheets.contains(&sheet));
    for r in r0..=r1 {
        for c in c0..=c1 {
            let v = st.model.get_formatted_cell_value(sheet, r, c)?;
            let style = if has_cf {
                // 含条件格式叠加后的有效样式（查邻格边框合并）
                st.model.get_extended_cell_style(sheet, r, c)?.style
            } else {
                // 轻量路径：仅取本格样式，不做邻格边框查询
                st.model
                    .get_model()
                    .get_cell_style_or_none(sheet, r, c)?
                    .unwrap_or_default()
            };
            let sj = style_to_json(st, &style);
            let has_style = sj["b"].as_bool() == Some(true)
                || sj["i"].as_bool() == Some(true)
                || sj["u"].as_bool() == Some(true)
                || sj["st"].as_bool() == Some(true)
                || sj["fc"].as_str().map(|x| !x.is_empty()).unwrap_or(false)
                || sj["bg"].as_str().map(|x| !x.is_empty()).unwrap_or(false)
                || sj["ha"].as_str() != Some("general")
                || sj["wr"].as_bool() == Some(true)
                || sj["sz"].as_i64().map(|x| x != 12).unwrap_or(false)
                || sj["fn"].as_str().map(|x| x != "Inter").unwrap_or(false)
                || !sj["br"]["t"].is_null()
                || !sj["br"]["b"].is_null()
                || !sj["br"]["l"].is_null()
                || !sj["br"]["r"].is_null();
            if v.is_empty() && !has_style {
                continue;
            }
            // 仅非空/有样式格才做额外调用
            let t = format!("{:?}", st.model.get_cell_type(sheet, r, c)?);
            // 公式原文（供「显示公式」模式与追踪引用箭头）
            let content = st.model.get_cell_content(sheet, r, c)?;
            let f = if content.starts_with('=') {
                content.as_str()
            } else {
                ""
            };
            let rich = st
                .rich_text
                .get(&(sheet, r, c))
                .map(|runs| rich_runs_json(st, runs));
            cells.push(json!({ "r": r, "c": c, "v": v, "t": t, "s": sj, "f": f, "rt": rich }));
        }
    }
    let mut col_widths = Vec::new();
    for c in c0..=c1 {
        col_widths.push(st.model.get_column_width(sheet, c)?);
    }
    let mut row_heights = Vec::new();
    for r in r0..=r1 {
        row_heights.push(st.model.get_row_height(sheet, r)?);
    }
    ok_json(json!({
        "cells": cells,
        "r0": r0, "c0": c0, "r1": r1, "c1": c1,
        "colWidths": col_widths,
        "rowHeights": row_heights,
        "frozenRows": st.model.get_frozen_rows_count(sheet)?,
        "frozenCols": st.model.get_frozen_columns_count(sheet)?,
        "merges": merges_json(st, sheet)?,
        "canUndo": !st.app_undo.is_empty(),
        "canRedo": !st.app_redo.is_empty(),
    }))
}

// 合并单元格清单 → [{r0,c0,r1,c1}]（解析引擎的 "A1:B2" 形式）
fn merged_ranges(st: &AppState, sheet: u32) -> Result<Vec<(i32, i32, i32, i32)>, String> {
    let mut out = Vec::new();
    for m in st.model.get_merged_cells(sheet)? {
        if let Some((a, b)) = m.split_once(':') {
            if let (Some((r0, c0)), Some((r1, c1))) = (parse_a1(a), parse_a1(b)) {
                out.push((r0.min(r1), c0.min(c1), r0.max(r1), c0.max(c1)));
            }
        }
    }
    Ok(out)
}

fn merged_anchor_in(ranges: &[(i32, i32, i32, i32)], row: i32, col: i32) -> (i32, i32) {
    ranges
        .iter()
        .find(|&&(r0, c0, r1, c1)| row >= r0 && row <= r1 && col >= c0 && col <= c1)
        .map(|&(r0, c0, _, _)| (r0, c0))
        .unwrap_or((row, col))
}

fn merges_json(st: &AppState, sheet: u32) -> Result<Value, String> {
    Ok(json!(
        merged_ranges(st, sheet)?
            .into_iter()
            .map(|(r0, c0, r1, c1)| json!({ "r0": r0, "c0": c0, "r1": r1, "c1": c1 }))
            .collect::<Vec<_>>()
    ))
}

fn parse_a1(s: &str) -> Option<(i32, i32)> {
    let mut col = 0i64;
    let mut row = String::new();
    for ch in s.chars() {
        if ch == '$' {
            continue;
        } else if ch.is_ascii_alphabetic() {
            if !row.is_empty() {
                return None;
            }
            col = col * 26 + (ch.to_ascii_uppercase() as i64 - 'A' as i64 + 1);
        } else if ch.is_ascii_digit() {
            row.push(ch);
        } else {
            return None;
        }
    }
    let r: i32 = row.parse().ok()?;
    if col < 1 || col > MAX_COLS as i64 {
        return None;
    }
    Some((r, col as i32))
}

fn rich_runs_json(st: &AppState, runs: &[RichTextRun]) -> Value {
    Value::Array(
        runs.iter()
            .map(|run| {
                let resolved = st.model.resolve_color(&run.color);
                json!({
                    "text": run.text,
                    "bold": run.bold,
                    "italic": run.italic,
                    "underline": run.underline,
                    "strike": run.strike,
                    "size": run.size,
                    "font": run.font,
                    // Keep the OOXML color token: "#RRGGBB", [themeIndex, tint], or null.
                    // resolvedColor exists only for painting the editor surface.
                    "color": &run.color,
                    "resolvedColor": if resolved.is_empty() { Value::Null } else { json!(resolved) },
                })
            })
            .collect(),
    )
}

#[derive(Debug)]
struct RichTextRunInput {
    text: String,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    size: Option<Option<f64>>,
    font: Option<Option<String>>,
    color: Option<Color>,
}

fn parse_rich_color(value: &Value) -> Result<Color, String> {
    match value {
        Value::Null => Ok(Color::None),
        Value::String(value) if value.is_empty() => Ok(Color::None),
        Value::String(value) => {
            let normalized = value.to_ascii_uppercase();
            Color::from_rgb(&normalized)
                .map_err(|_| format!("invalid rich-text RGB color: {value}"))
        }
        Value::Array(values) if values.len() == 2 => {
            let index = values[0]
                .as_i64()
                .ok_or("rich-text theme color index must be an integer")?;
            let tint = values[1]
                .as_f64()
                .ok_or("rich-text theme color tint must be a number")?;
            if !(0..=11).contains(&index) {
                return Err("rich-text theme color index must be between 0 and 11".into());
            }
            if !tint.is_finite() || !(-1.0..=1.0).contains(&tint) {
                return Err("rich-text theme tint must be finite and between -1 and 1".into());
            }
            Ok(Color::Theme(index as i32, tint))
        }
        Value::Object(values) => {
            let index = values
                .get("theme")
                .or_else(|| values.get("index"))
                .and_then(Value::as_i64)
                .ok_or("rich-text theme color object requires theme/index")?;
            let tint = values.get("tint").and_then(Value::as_f64).unwrap_or(0.0);
            if !(0..=11).contains(&index) {
                return Err("rich-text theme color index must be between 0 and 11".into());
            }
            if !tint.is_finite() || !(-1.0..=1.0).contains(&tint) {
                return Err("rich-text theme tint must be finite and between -1 and 1".into());
            }
            Ok(Color::Theme(index as i32, tint))
        }
        _ => Err("rich-text color must be #RRGGBB, [theme,tint], a theme object, or null".into()),
    }
}

fn parse_optional_rich_bool(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, String> {
    object
        .get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| format!("rich-text run field {key} must be boolean"))
        })
        .transpose()
}

fn parse_rich_run_input(value: &Value) -> Result<RichTextRunInput, String> {
    let object = value
        .as_object()
        .ok_or("each rich-text run must be an object")?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "text"
                | "bold"
                | "italic"
                | "underline"
                | "strike"
                | "size"
                | "font"
                | "color"
                | "resolvedColor"
        ) {
            return Err(format!("unknown rich-text run field: {key}"));
        }
    }
    let text = object
        .get("text")
        .and_then(Value::as_str)
        .ok_or("rich-text run text must be a string")?
        .to_string();
    let size = object
        .get("size")
        .map(|value| -> Result<Option<f64>, String> {
            if value.is_null() {
                Ok(None)
            } else {
                let value = value
                    .as_f64()
                    .ok_or("rich-text run size must be a number or null")?;
                Ok(Some(value))
            }
        })
        .transpose()?;
    let font = object
        .get("font")
        .map(|value| -> Result<Option<String>, String> {
            if value.is_null() {
                Ok(None)
            } else {
                Ok(Some(
                    value
                        .as_str()
                        .ok_or("rich-text run font must be a string or null")?
                        .to_string(),
                ))
            }
        })
        .transpose()?;
    let color = object.get("color").map(parse_rich_color).transpose()?;
    Ok(RichTextRunInput {
        text,
        bold: parse_optional_rich_bool(object, "bold")?,
        italic: parse_optional_rich_bool(object, "italic")?,
        underline: parse_optional_rich_bool(object, "underline")?,
        strike: parse_optional_rich_bool(object, "strike")?,
        size,
        font,
        color,
    })
}

fn xml_10_text_is_valid(value: &str) -> bool {
    value.chars().all(|ch| {
        matches!(ch, '\u{9}' | '\u{A}' | '\u{D}')
            || ('\u{20}'..='\u{D7FF}').contains(&ch)
            || ('\u{E000}'..='\u{FFFD}').contains(&ch)
            || ('\u{10000}'..='\u{10FFFF}').contains(&ch)
    })
}

fn validate_rich_run(run: &RichTextRun) -> Result<(), String> {
    if run.text.is_empty() {
        return Err("rich-text runs cannot have empty text".into());
    }
    if !xml_10_text_is_valid(&run.text) {
        return Err("rich-text contains characters forbidden by XML 1.0".into());
    }
    if let Some(size) = run.size {
        if !size.is_finite() || !(1.0..=409.0).contains(&size) {
            return Err("rich-text font size must be finite and between 1 and 409 points".into());
        }
    }
    if let Some(font) = &run.font {
        if font.is_empty() || font.chars().count() > 255 || !xml_10_text_is_valid(font) {
            return Err("rich-text font name must contain 1 to 255 valid XML characters".into());
        }
    }
    match &run.color {
        Color::Rgb(value) => {
            Color::from_rgb(value).map_err(|_| format!("invalid rich-text RGB color: {value}"))?;
        }
        Color::Theme(index, tint) => {
            if !(0..=11).contains(index) || !tint.is_finite() || !(-1.0..=1.0).contains(tint) {
                return Err("invalid rich-text theme color".into());
            }
        }
        Color::None => {}
    }
    Ok(())
}

fn rich_run_from_style(style: &Style) -> RichTextRun {
    RichTextRun {
        text: String::new(),
        bold: style.font.b,
        italic: style.font.i,
        underline: style.font.u,
        strike: style.font.strike,
        size: Some(style.font.sz as f64),
        font: Some(style.font.name.clone()),
        color: style.font.color.clone(),
    }
}

fn rich_run_source_map(old_runs: &[RichTextRun], new_texts: &[String]) -> Vec<Option<usize>> {
    #[derive(Clone, Copy)]
    struct Span {
        start: usize,
        end: usize,
    }
    let mut cursor = 0usize;
    let old_spans = old_runs
        .iter()
        .map(|run| {
            let start = cursor;
            cursor += run.text.chars().count();
            Span { start, end: cursor }
        })
        .collect::<Vec<_>>();
    let old_len = cursor;
    cursor = 0;
    let new_spans = new_texts
        .iter()
        .map(|text| {
            let start = cursor;
            cursor += text.chars().count();
            Span { start, end: cursor }
        })
        .collect::<Vec<_>>();
    let new_len = cursor;
    let old_text = old_runs
        .iter()
        .map(|run| run.text.as_str())
        .collect::<String>();
    let new_text = new_texts.iter().map(String::as_str).collect::<String>();
    let prefix = old_text
        .chars()
        .zip(new_text.chars())
        .take_while(|(old, new)| old == new)
        .count();
    let suffix = old_text
        .chars()
        .rev()
        .zip(new_text.chars().rev())
        .take_while(|(old, new)| old == new)
        .count()
        .min(old_len.saturating_sub(prefix))
        .min(new_len.saturating_sub(prefix));
    let suffix_new_start = new_len.saturating_sub(suffix);
    let suffix_old_start = old_len.saturating_sub(suffix);
    let overlap = |a: Span, b: Span| a.end.min(b.end).saturating_sub(a.start.max(b.start));

    let mut result = Vec::with_capacity(new_spans.len());
    for (new_index, new_span) in new_spans.iter().copied().enumerate() {
        let mut best = None;
        let mut best_score = 0usize;
        for (old_index, old_span) in old_spans.iter().copied().enumerate() {
            let mut score = 0usize;
            // Characters in the common prefix have identical absolute coordinates.
            score += overlap(
                Span {
                    start: new_span.start,
                    end: new_span.end.min(prefix),
                },
                old_span,
            );
            // Characters in the common suffix are translated by the insertion/deletion delta.
            let suffix_part = Span {
                start: new_span.start.max(suffix_new_start),
                end: new_span.end,
            };
            if suffix_part.end > suffix_part.start {
                let translated = Span {
                    start: suffix_old_start + (suffix_part.start - suffix_new_start),
                    end: suffix_old_start + (suffix_part.end - suffix_new_start),
                };
                score += overlap(translated, old_span);
            }
            // An unchanged run moved within the edited middle is still a strong template anchor.
            if score == 0 && new_texts[new_index] == old_runs[old_index].text {
                score = new_texts[new_index].chars().count().max(1);
            }
            if score > best_score {
                best = Some(old_index);
                best_score = score;
            }
        }
        if best.is_none() {
            // Inserted text inherits the adjacent source run. This mirrors Excel's typing style
            // and, crucially, does not advance every later template by one index.
            best = result
                .last()
                .copied()
                .flatten()
                .or_else(|| old_spans.iter().position(|span| new_span.start <= span.end));
        }
        result.push(best);
    }
    result
}

fn normalize_rich_run_input(
    st: &AppState,
    input: RichTextRunInput,
    inherited: &RichTextRun,
) -> RichTextRun {
    let mut color = input.color.unwrap_or_else(|| inherited.color.clone());
    // A display-only RGB round trip from an older UI is still an unchanged theme token.
    if matches!(inherited.color, Color::Theme(_, _)) {
        if let Color::Rgb(rgb) = &color {
            if rgb.eq_ignore_ascii_case(&st.model.resolve_color(&inherited.color)) {
                color = inherited.color.clone();
            }
        }
    }
    RichTextRun {
        text: input.text,
        bold: input.bold.unwrap_or(inherited.bold),
        italic: input.italic.unwrap_or(inherited.italic),
        underline: input.underline.unwrap_or(inherited.underline),
        strike: input.strike.unwrap_or(inherited.strike),
        size: input.size.unwrap_or(inherited.size),
        font: input.font.unwrap_or_else(|| inherited.font.clone()),
        color,
    }
}

fn api_rich_text(st: &mut AppState, query: &str, body: &[u8]) -> Result<Resp, String> {
    if body.is_empty() {
        let sheet = qi(query, "sheet", 0) as u32;
        let requested_row = qi(query, "row", 1).clamp(1, MAX_ROWS);
        let requested_col = qi(query, "col", 1).clamp(1, MAX_COLS);
        let (row, col) = merged_anchor_in(&merged_ranges(st, sheet)?, requested_row, requested_col);
        let runs = st
            .rich_text
            .get(&(sheet, row, col))
            .map(|runs| rich_runs_json(st, runs))
            .unwrap_or_else(|| json!([]));
        return ok_json(json!({
            "sheet": sheet,
            "row": row,
            "col": col,
            "mergedAnchor": row != requested_row || col != requested_col,
            "content": st.model.get_cell_content(sheet, row, col)?,
            "runs": runs,
        }));
    }

    let value = parse_body(body)?;
    let sheet = ji(&value, "sheet")? as u32;
    let requested_row = ji(&value, "row")? as i32;
    let requested_col = ji(&value, "col")? as i32;
    let (row, col) = merged_anchor_in(&merged_ranges(st, sheet)?, requested_row, requested_col);
    let input_runs = value
        .get("runs")
        .and_then(Value::as_array)
        .ok_or("rich-text request requires a runs array")?;
    if input_runs.len() > 32_767 {
        return Err("rich-text run count exceeds Excel's limit".into());
    }

    let key = (sheet, row, col);
    let old_runs = st.rich_text.get(&key).cloned().unwrap_or_default();
    let base_run = rich_run_from_style(&st.model.get_cell_style(sheet, row, col)?);
    let parsed_inputs = input_runs
        .iter()
        .map(parse_rich_run_input)
        .collect::<Result<Vec<_>, _>>()?;
    let new_texts = parsed_inputs
        .iter()
        .map(|input| input.text.clone())
        .collect::<Vec<_>>();
    let source_map = rich_run_source_map(&old_runs, &new_texts);
    let mut runs = Vec::with_capacity(input_runs.len());
    for (index, input) in parsed_inputs.into_iter().enumerate() {
        let inherited = source_map[index]
            .and_then(|source| old_runs.get(source))
            .unwrap_or(&base_run);
        let run = normalize_rich_run_input(st, input, inherited);
        validate_rich_run(&run)?;
        runs.push(run);
    }
    let content = runs.iter().map(|run| run.text.as_str()).collect::<String>();
    if content.encode_utf16().count() > 32_767 {
        return Err("rich-text content exceeds Excel's 32,767 character limit".into());
    }
    if let Some(expected) = value.get("content") {
        let expected = expected
            .as_str()
            .ok_or("rich-text content must be a string")?;
        if expected != content {
            return Err("rich-text runs do not concatenate to content".into());
        }
    }

    if old_runs == runs && st.model.get_cell_content(sheet, row, col)? == content {
        return ok_json(json!({
            "sheet": sheet,
            "row": row,
            "col": col,
            "content": content,
            "runs": rich_runs_json(st, &runs),
            "unchanged": true,
            "richTextPreserved": true,
        }));
    }

    let rich_xml = if runs.is_empty() {
        None
    } else {
        Some(build_rich_shared_item_xml(
            st.rich_text_xml.get(&key).map(String::as_str),
            &old_runs,
            &runs,
        )?)
    };
    st.model
        .set_rich_text_plain_value(sheet, row, col, &content)?;
    if runs.is_empty() {
        st.rich_text.remove(&key);
        st.rich_text_xml.remove(&key);
    } else {
        st.rich_text.insert(key, runs.clone());
        st.rich_text_xml
            .insert(key, rich_xml.expect("rich XML exists"));
    }
    let formatted = st.model.get_formatted_cell_value(sheet, row, col)?;
    ok_json(json!({
        "sheet": sheet,
        "row": row,
        "col": col,
        "mergedAnchor": row != requested_row || col != requested_col,
        "content": content,
        "formatted": formatted,
        "runs": rich_runs_json(st, &runs),
    }))
}

fn api_cell(st: &AppState, query: &str) -> Result<Resp, String> {
    let sheet = qi(query, "sheet", 0) as u32;
    let requested_row = qi(query, "row", 1).clamp(1, MAX_ROWS);
    let requested_col = qi(query, "col", 1).clamp(1, MAX_COLS);
    let ranges = merged_ranges(st, sheet)?;
    let (row, col) = merged_anchor_in(&ranges, requested_row, requested_col);
    let content = st.model.get_cell_content(sheet, row, col)?;
    let formatted = st.model.get_formatted_cell_value(sheet, row, col)?;
    let value = match st
        .model
        .get_model()
        .get_cell_value_by_index(sheet, row, col)?
    {
        CellValue::Number(value) => json!(value),
        CellValue::String(value) => Value::String(value),
        CellValue::Boolean(value) => Value::Bool(value),
        CellValue::None => Value::Null,
    };
    let style = st.model.get_cell_style(sheet, row, col)?;
    let rich_text = st
        .rich_text
        .get(&(sheet, row, col))
        .map(|runs| rich_runs_json(st, runs));
    ok_json(json!({
        "content": content,
        "value": value,
        "formatted": formatted,
        "style": style_to_json(st, &style),
        "richText": rich_text,
        "hasRichText": rich_text
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|runs| !runs.is_empty()),
        "row": row,
        "col": col,
        "mergedAnchor": row != requested_row || col != requested_col,
    }))
}

fn ai_sheet_index(
    names: &[String],
    value: Option<&Value>,
    default_sheet: u32,
) -> Result<u32, String> {
    match value {
        None => Ok(default_sheet),
        Some(Value::String(name)) => names
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(name))
            .map(|index| index as u32)
            .ok_or_else(|| format!("工作表不存在：{name}")),
        Some(Value::Number(index)) => {
            let index = index.as_u64().ok_or("sheet 索引必须是非负整数")? as usize;
            (index < names.len())
                .then_some(index as u32)
                .ok_or_else(|| format!("工作表索引越界：{index}"))
        }
        Some(_) => Err("sheet 必须是工作表名或零基索引".into()),
    }
}

fn ai_resolve_reference(
    value: &Value,
    names: &[String],
    default_sheet: u32,
) -> Result<(u32, ai_context::ParsedRange), String> {
    let reference = value
        .get("ref")
        .and_then(Value::as_str)
        .ok_or("缺少 ref（A1 地址）")?;
    let parsed = ai_context::parse_range(reference)
        .ok_or_else(|| format!("无法解析的 A1 地址：{reference}"))?;
    let sheet = match parsed.sheet_name.as_deref() {
        Some(name) => names
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(name))
            .map(|index| index as u32)
            .ok_or_else(|| format!("工作表不存在：{name}"))?,
        None => default_sheet,
    };
    Ok((sheet, parsed))
}

fn ai_structure_metadata(st: &AppState, names: &[String]) -> Result<Value, String> {
    let mut tables = st
        .model
        .get_tables()
        .into_values()
        .map(|table| {
            json!({
                "name": table.display_name,
                "sheet": table.sheet_name,
                "ref": table.reference,
                "columns": table.columns.into_iter().map(|column| column.name).collect::<Vec<_>>(),
                "headerRows": table.header_row_count,
                "totalRows": table.totals_row_count,
            })
        })
        .collect::<Vec<_>>();
    tables.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));

    let defined_names = st
        .model
        .get_defined_name_list()
        .into_iter()
        .map(|(name, scope, formula)| json!({"name":name,"scope":scope,"formula":formula}))
        .collect::<Vec<_>>();

    let mut conditional_formats = Vec::with_capacity(names.len());
    let mut data_validations = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let sheet = index as u32;
        let rules = st.model.get_conditional_formatting_list(sheet)?;
        let compact_rules = rules
            .iter()
            .map(|rule| {
                let encoded = serde_json::to_value(&rule.cf_rule).unwrap_or(Value::Null);
                json!({
                    "range": rule.range,
                    "type": encoded.get("type").and_then(Value::as_str).unwrap_or("unknown"),
                    "priority": rule.priority,
                })
            })
            .collect::<Vec<_>>();
        conditional_formats.push(json!({
            "sheet": name,
            "count": compact_rules.len(),
            "rules": compact_rules,
        }));

        let validation_rules = st
            .worksheet_features
            .data_validations
            .get(&sheet)
            .map(|transport| {
                transport
                    .rules
                    .iter()
                    .map(|rule| {
                        json!({
                            "id": rule.id,
                            "ref": rule.sqref,
                            "type": rule.validation_type,
                            "operator": rule.operator,
                            "allowBlank": rule.allow_blank,
                            "formula1": rule.formula1,
                            "formula2": rule.formula2,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        data_validations.push(json!({
            "sheet": name,
            "count": validation_rules.len(),
            "rules": validation_rules,
        }));
    }

    let pivot_tables = pivot_table_model_with_edits(st)
        .ok()
        .and_then(|model| model.get("tables").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|table| {
            json!({
                "name": table.get("name").cloned().unwrap_or(Value::Null),
                "sheet": table.get("sheet").cloned().unwrap_or(Value::Null),
                "location": table.get("location").cloned().unwrap_or(Value::Null),
                "cacheId": table.get("cacheId").cloned().unwrap_or(Value::Null),
                "part": table.get("part").cloned().unwrap_or(Value::Null),
                "cachePart": table.get("cachePart").cloned().unwrap_or(Value::Null),
                "model": table,
            })
        })
        .collect::<Vec<_>>();

    // Native chart object ids are the stable handles accepted by the typed AI updateChart op.
    // Include the full parsed model: U AI is capability-first, and can inspect/edit the same
    // lossless differential representation used by the visual chart editor.
    let mut charts = Vec::new();
    let mut chart_sheets = st.objects.keys().copied().collect::<Vec<_>>();
    chart_sheets.sort_unstable();
    for sheet in chart_sheets {
        let mut objects = st
            .objects
            .get(&sheet)
            .into_iter()
            .flatten()
            .filter(|object| {
                object
                    .get("config")
                    .and_then(|config| config.get("nativeDrawing"))
                    .and_then(|descriptor| descriptor.get("kind"))
                    .and_then(Value::as_str)
                    == Some("chart")
            })
            .collect::<Vec<_>>();
        objects.sort_by(|left, right| {
            left.get("id")
                .and_then(Value::as_str)
                .cmp(&right.get("id").and_then(Value::as_str))
        });
        for object in objects {
            let id = object.get("id").cloned().unwrap_or(Value::Null);
            let model = ai_chart_model_from_object(st, object)
                .unwrap_or_else(|error| json!({"error":error}));
            charts.push(json!({
                "id": id,
                "sheet": names.get(sheet as usize).cloned().unwrap_or_default(),
                "bounds": object.get("bounds").cloned().unwrap_or(Value::Null),
                "model": model,
            }));
        }
    }

    Ok(json!({
        "tables": tables,
        "charts": charts,
        "pivotTables": pivot_tables,
        "definedNames": defined_names,
        "conditionalFormats": conditional_formats,
        "dataValidations": data_validations,
    }))
}

fn ai_workbook_digest(st: &AppState) -> Result<Value, String> {
    let names = st.model.get_model().workbook.get_worksheet_names();
    let mut digest = ai_context::workbook_digest(&st.model)?;
    let structure = ai_structure_metadata(st, &names)?;
    for key in [
        "tables",
        "charts",
        "pivotTables",
        "definedNames",
        "conditionalFormats",
        "dataValidations",
    ] {
        digest[key] = structure[key].clone();
    }
    Ok(digest)
}

fn ai_sheet_digest(st: &AppState, sheet: u32, names: &[String]) -> Result<Value, String> {
    let mut digest = ai_context::sheet_digest(&st.model, sheet, &names[sheet as usize])?;
    let structure = ai_structure_metadata(st, names)?;
    let sheet_name = &names[sheet as usize];
    digest["tables"] = Value::Array(
        structure["tables"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|table| table["sheet"].as_str() == Some(sheet_name))
            .cloned()
            .collect(),
    );
    digest["charts"] = Value::Array(
        structure["charts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|chart| chart["sheet"].as_str() == Some(sheet_name))
            .cloned()
            .collect(),
    );
    digest["pivotTables"] = Value::Array(
        structure["pivotTables"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|table| table["sheet"].as_str() == Some(sheet_name))
            .cloned()
            .collect(),
    );
    digest["definedNames"] = structure["definedNames"].clone();
    digest["conditionalFormats"] = Value::Array(
        structure["conditionalFormats"]
            .as_array()
            .and_then(|items| items.get(sheet as usize))
            .cloned()
            .into_iter()
            .collect(),
    );
    digest["dataValidations"] = Value::Array(
        structure["dataValidations"]
            .as_array()
            .and_then(|items| items.get(sheet as usize))
            .cloned()
            .into_iter()
            .collect(),
    );
    Ok(digest)
}

fn api_ai_context(st: &AppState, body: &[u8]) -> Result<Resp, String> {
    let request = parse_body(body)?;
    let op = js(&request, "op")?;
    let names = st.model.get_model().workbook.get_worksheet_names();
    let selected_sheet = st.model.get_selected_view().sheet;
    let default_sheet = ai_sheet_index(&names, request.get("sheet"), selected_sheet)?;
    let result = match op {
        "digest" => {
            if request.get("sheet").is_some() {
                ai_sheet_digest(st, default_sheet, &names)?
            } else {
                ai_workbook_digest(st)?
            }
        }
        "slice" => {
            let (sheet, range) = ai_resolve_reference(&request, &names, default_sheet)?;
            ai_context::slice(
                &st.model,
                sheet,
                &names[sheet as usize],
                range.r0,
                range.c0,
                range.r1,
                range.c1,
            )?
        }
        "detail" => {
            let (sheet, range) = ai_resolve_reference(&request, &names, default_sheet)?;
            if range.r0 != range.r1 || range.c0 != range.c1 {
                return Err("detail 只接受单个单元格引用".into());
            }
            ai_context::detail(&st.model, sheet, &names[sheet as usize], range.r0, range.c0)?
        }
        "errors" => {
            let limit = request
                .get("limit")
                .map(|value| {
                    value
                        .as_u64()
                        .filter(|limit| *limit > 0)
                        .ok_or("limit 必须是正整数")
                })
                .transpose()?
                .unwrap_or(50)
                .min(500) as usize;
            json!({
                "sheet": names[default_sheet as usize],
                "errors": ai_context::scan_errors(
                    &st.model,
                    default_sheet,
                    &names[default_sheet as usize],
                    limit,
                )?,
            })
        }
        other => {
            return Err(format!(
                "不支持的 AI context 操作 {other}；可用：digest / slice / detail / errors"
            ));
        }
    };
    ok_json(result)
}

fn ai_op_at(op: &ai_context::CellOp, row: i32, col: i32) -> ai_context::CellOp {
    match op {
        ai_context::CellOp::SetValue { sheet, value, .. } => ai_context::CellOp::SetValue {
            sheet: *sheet,
            row,
            col,
            value: value.clone(),
        },
        ai_context::CellOp::SetFormula { sheet, formula, .. } => ai_context::CellOp::SetFormula {
            sheet: *sheet,
            row,
            col,
            formula: formula.clone(),
        },
        ai_context::CellOp::Clear { sheet, .. } => ai_context::CellOp::Clear {
            sheet: *sheet,
            row,
            col,
        },
    }
}

fn ai_resolve_merged_ops(
    st: &AppState,
    ops: &[ai_context::CellOp],
) -> Result<Vec<ai_context::CellOp>, String> {
    let mut merges = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        let (sheet, row, col) = op.target();
        let ranges = match merges.entry(sheet) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(merged_ranges(st, sheet)?)
            }
        };
        let (row, col) = merged_anchor_in(ranges, row, col);
        out.push(ai_op_at(op, row, col));
    }
    Ok(out)
}

#[derive(Debug, Clone)]
enum AiTypedOp {
    Cells(Vec<ai_context::CellOp>),
    SetFormat {
        sheet: u32,
        reference: String,
        range: [i32; 4],
        styles: Vec<(String, String)>,
    },
    SetBorder {
        sheet: u32,
        reference: String,
        range: [i32; 4],
        border_type: String,
        style: String,
        color: String,
    },
    UpdateChart {
        sheet: u32,
        id: String,
        patch: Value,
    },
    UpdatePivotTable {
        part: String,
        patch: Value,
    },
}

fn ai_style_path(name: &str) -> Option<&'static str> {
    match name {
        "font.bold" | "font.b" => Some("font.b"),
        "font.italic" | "font.i" => Some("font.i"),
        "font.underline" | "font.u" => Some("font.u"),
        "font.strike" => Some("font.strike"),
        "font.size" => Some("font.size"),
        "font.name" => Some("font.name"),
        "font.color" => Some("font.color"),
        "fill.color" => Some("fill.color"),
        "alignment.horizontal" => Some("alignment.horizontal"),
        "alignment.vertical" => Some("alignment.vertical"),
        "alignment.wrapText" | "alignment.wrap_text" => Some("alignment.wrap_text"),
        "numberFormat" | "num_fmt" => Some("num_fmt"),
        _ => None,
    }
}

fn ai_style_value(value: &Value) -> Result<String, String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err("格式值必须是字符串、数字或布尔值".to_string()),
    }
}

fn ai_parse_typed_ops(
    st: &AppState,
    items: &[Value],
    default_sheet: u32,
    names: &[String],
) -> Result<Vec<AiTypedOp>, String> {
    if items.len() > 128 {
        return Err("单次 AI 请求最多包含 128 个顶层操作".to_string());
    }
    let resolve_sheet = |name: &str| {
        names
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(name))
            .map(|index| index as u32)
    };
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let at = |message: String| format!("ops[{index}]: {message}");
        let kind = item
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| at("缺少 op".to_string()))?;
        match kind {
            "setValue" | "setFormula" | "setRange" | "clear" => {
                let parsed = ai_context::parse_ops(
                    std::slice::from_ref(item),
                    default_sheet,
                    resolve_sheet,
                )?;
                out.push(AiTypedOp::Cells(ai_resolve_merged_ops(st, &parsed)?));
            }
            "setFormat" | "setBorder" => {
                let reference = item
                    .get("ref")
                    .and_then(Value::as_str)
                    .ok_or_else(|| at("缺少 ref".to_string()))?;
                let parsed = ai_context::parse_range(reference)
                    .ok_or_else(|| at(format!("无法解析的 A1 地址：{reference}")))?;
                let sheet = match parsed.sheet_name.as_deref() {
                    Some(name) => {
                        resolve_sheet(name).ok_or_else(|| at(format!("工作表不存在：{name}")))?
                    }
                    None => default_sheet,
                };
                let range = [parsed.r0, parsed.c0, parsed.r1, parsed.c1];
                if kind == "setFormat" {
                    let style = item
                        .get("style")
                        .and_then(Value::as_object)
                        .ok_or_else(|| at("setFormat 缺少 style 对象".to_string()))?;
                    if style.is_empty() {
                        return Err(at("setFormat.style 不能为空".to_string()));
                    }
                    let mut styles = Vec::with_capacity(style.len());
                    for (name, value) in style {
                        let path = ai_style_path(name)
                            .ok_or_else(|| at(format!("不支持的格式字段：{name}")))?;
                        styles.push((path.to_string(), ai_style_value(value).map_err(&at)?));
                    }
                    out.push(AiTypedOp::SetFormat {
                        sheet,
                        reference: ai_context::qualify(
                            &names[sheet as usize],
                            parsed.r0,
                            parsed.c0,
                            parsed.r1,
                            parsed.c1,
                        ),
                        range,
                        styles,
                    });
                } else {
                    let border_type = item
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("all")
                        .to_string();
                    if !matches!(
                        border_type.as_str(),
                        "all"
                            | "inner"
                            | "outer"
                            | "top"
                            | "right"
                            | "bottom"
                            | "left"
                            | "centerh"
                            | "centerv"
                            | "none"
                    ) {
                        return Err(at(format!("不支持的边框类型：{border_type}")));
                    }
                    out.push(AiTypedOp::SetBorder {
                        sheet,
                        reference: ai_context::qualify(
                            &names[sheet as usize],
                            parsed.r0,
                            parsed.c0,
                            parsed.r1,
                            parsed.c1,
                        ),
                        range,
                        border_type,
                        style: item
                            .get("style")
                            .and_then(Value::as_str)
                            .unwrap_or("thin")
                            .to_string(),
                        color: item
                            .get("color")
                            .and_then(Value::as_str)
                            .unwrap_or("#000000")
                            .to_string(),
                    });
                }
            }
            "updateChart" => {
                let sheet = ai_sheet_index(names, item.get("sheet"), default_sheet).map_err(&at)?;
                let id = item
                    .get("chartId")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| at("updateChart 缺少 chartId".to_string()))?
                    .to_string();
                let patch = item
                    .get("patch")
                    .filter(|value| value.as_object().is_some_and(|map| !map.is_empty()))
                    .cloned()
                    .ok_or_else(|| at("updateChart 缺少非空 patch 对象".to_string()))?;
                out.push(AiTypedOp::UpdateChart { sheet, id, patch });
            }
            "updatePivotTable" => {
                let part = item
                    .get("part")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| at("updatePivotTable 缺少 part".to_string()))?
                    .to_string();
                let patch = item
                    .get("patch")
                    .filter(|value| value.as_object().is_some_and(|map| !map.is_empty()))
                    .cloned()
                    .ok_or_else(|| at("updatePivotTable 缺少非空 patch 对象".to_string()))?;
                out.push(AiTypedOp::UpdatePivotTable { part, patch });
            }
            other => {
                return Err(at(format!(
                    "不支持的操作 {other}；可用：setValue / setFormula / setRange / clear / setFormat / setBorder / updateChart / updatePivotTable"
                )));
            }
        }
    }
    Ok(out)
}

fn ai_deep_merge(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                if value.is_null() {
                    target.insert(key.clone(), Value::Null);
                } else if let Some(existing) = target.get_mut(key) {
                    ai_deep_merge(existing, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

fn ai_chart_model_from_object(st: &AppState, object: &Value) -> Result<Value, String> {
    let descriptor = object
        .get("config")
        .and_then(|config| config.get("nativeDrawing"))
        .ok_or("chart has no nativeDrawing descriptor")?;
    if descriptor.get("kind").and_then(Value::as_str) != Some("chart") {
        return Err("target object is not a native chart".to_string());
    }
    let part = descriptor
        .get("contentPart")
        .and_then(Value::as_str)
        .ok_or("native chart content part is missing")?;
    let original = st
        .source_ooxml
        .as_ref()
        .and_then(|snapshot| snapshot.parts.get(part))
        .ok_or_else(|| format!("native chart content part {part} is unavailable"))?;
    let original = std::str::from_utf8(original)
        .map_err(|error| format!("native chart content UTF-8: {error}"))?;
    let current = if let Some(edit) = native_descriptor_edit(descriptor, "chart") {
        native_chart_edit::apply_chart_edit(original, edit)?
    } else {
        original.to_string()
    };
    Ok(native_chart_edit::parse_chart_model(&current))
}

fn ai_update_chart(st: &mut AppState, sheet: u32, id: &str, patch: &Value) -> Result<(), String> {
    let position = st
        .objects
        .get(&sheet)
        .and_then(|objects| {
            objects
                .iter()
                .position(|object| object.get("id").and_then(Value::as_str) == Some(id))
        })
        .ok_or_else(|| format!("找不到原生图表：{id}"))?;
    let mut updated = st.objects[&sheet][position].clone();
    let descriptor = updated
        .get_mut("config")
        .and_then(Value::as_object_mut)
        .and_then(|config| config.get_mut("nativeDrawing"))
        .and_then(Value::as_object_mut)
        .ok_or("chart has no editable nativeDrawing descriptor")?;
    if descriptor.get("kind").and_then(Value::as_str) != Some("chart") {
        return Err(format!("对象 {id} 不是原生图表"));
    }
    let edits = descriptor
        .entry("edits".to_string())
        .or_insert_with(|| json!({}));
    if !edits.is_object() {
        *edits = json!({});
    }
    let chart_edit = edits
        .as_object_mut()
        .expect("edits was normalized to an object")
        .entry("chart".to_string())
        .or_insert_with(|| json!({}));
    ai_deep_merge(chart_edit, patch);
    let model = ai_chart_model_from_object(st, &updated)?;
    updated["config"]["nativeDrawing"]["model"] = model;
    st.objects.get_mut(&sheet).expect("chart sheet exists")[position] = updated;
    Ok(())
}

fn ai_preview_state(st: &AppState) -> Result<AppState, String> {
    let mut preview = AppState::new_with_storage_scope("ai-preview");
    preview.model = UserModel::from_bytes(&st.model.to_bytes(), "en")?;
    preview.file_name = st.file_name.clone();
    preview.source_ooxml = st.source_ooxml.clone();
    preview.excel_extension = st.excel_extension.clone();
    preview.excel_mime = st.excel_mime.clone();
    preview.restore_sidecars(&st.sidecar_snapshot());
    preview.clear_application_history();
    Ok(preview)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AiCellSnapshot {
    content: String,
    formatted: String,
    style: Value,
}

fn ai_diff_scope(
    model: &UserModel<'_>,
    ops: &[AiTypedOp],
) -> Result<(Vec<(u32, i32, i32)>, bool), String> {
    let limit = ai_context::MAX_DIFF_CELLS as usize;
    let mut cells = std::collections::BTreeSet::new();
    for op in ops {
        match op {
            AiTypedOp::Cells(items) => {
                for item in items {
                    cells.insert(item.target());
                }
            }
            AiTypedOp::SetFormat { sheet, range, .. }
            | AiTypedOp::SetBorder { sheet, range, .. } => {
                for row in range[0]..=range[2] {
                    for col in range[1]..=range[3] {
                        cells.insert((*sheet, row, col));
                        if cells.len() > limit {
                            break;
                        }
                    }
                    if cells.len() > limit {
                        break;
                    }
                }
            }
            AiTypedOp::UpdateChart { .. } | AiTypedOp::UpdatePivotTable { .. } => {}
        }
        if cells.len() > limit {
            return Err(format!(
                "操作涉及的单元格超过 diff 上限 {}，请拆成多次请求",
                ai_context::MAX_DIFF_CELLS
            ));
        }
    }
    let include_formula_dependents = ops
        .iter()
        .any(|op| matches!(op, AiTypedOp::Cells(items) if !items.is_empty()));
    let mut truncated = false;
    if include_formula_dependents {
        'sheets: for sheet in 0..model.get_model().workbook.worksheets.len() as u32 {
            let dimension = model
                .get_model()
                .workbook
                .worksheet(sheet)
                .map_err(|error| error.to_string())?
                .dimension();
            for row in dimension.min_row.max(1)..=dimension.max_row.max(1) {
                for col in dimension.min_column.max(1)..=dimension.max_column.max(1) {
                    let cell = (sheet, row, col);
                    if cells.contains(&cell) {
                        continue;
                    }
                    if cells.len() >= limit {
                        truncated = true;
                        break 'sheets;
                    }
                    cells.insert(cell);
                }
            }
        }
    }
    Ok((cells.into_iter().collect(), truncated))
}

fn ai_snapshot(
    st: &AppState,
    scope: &[(u32, i32, i32)],
) -> Result<std::collections::BTreeMap<(u32, i32, i32), AiCellSnapshot>, String> {
    let mut out = std::collections::BTreeMap::new();
    for &(sheet, row, col) in scope {
        out.insert(
            (sheet, row, col),
            AiCellSnapshot {
                content: st.model.get_cell_content(sheet, row, col)?,
                formatted: st.model.get_formatted_cell_value(sheet, row, col)?,
                style: style_to_json(st, &st.model.get_cell_style(sheet, row, col)?),
            },
        );
    }
    Ok(out)
}

fn ai_diff_json(
    before: &std::collections::BTreeMap<(u32, i32, i32), AiCellSnapshot>,
    after: &std::collections::BTreeMap<(u32, i32, i32), AiCellSnapshot>,
    names: &[String],
) -> Vec<Value> {
    before
        .iter()
        .filter_map(|(&(sheet, row, col), old)| {
            let new = after.get(&(sheet, row, col))?;
            (old != new).then(|| {
                json!({
                    "ref": ai_context::qualify(&names[sheet as usize], row, col, row, col),
                    "before": { "content": old.content, "formatted": old.formatted, "style": old.style },
                    "after": { "content": new.content, "formatted": new.formatted, "style": new.style },
                })
            })
        })
        .collect()
}

fn execute_ai_ops(model: &mut UserModel<'_>, ops: &[ai_context::CellOp]) -> Result<(), String> {
    model.pause_evaluation();
    let mut result = Ok(());
    for op in ops {
        let (sheet, row, col) = op.target();
        let write = match op {
            // Formula-looking literal text must never pass through the normal user-input parser.
            // The dedicated literal path also records an undo diff that restores the old cell
            // without leaving an orphan quote-prefix style on an originally empty cell.
            ai_context::CellOp::SetValue { value, .. }
                if value.starts_with('=') || value.starts_with('\'') =>
            {
                model.set_rich_text_plain_value(sheet, row, col, value)
            }
            _ => model.set_user_input(sheet, row, col, op.payload()),
        };
        if let Err(error) = write {
            result = Err(error);
            break;
        }
    }
    model.resume_evaluation();
    model.evaluate();
    result
}

fn execute_ai_typed_ops(st: &mut AppState, ops: &[AiTypedOp]) -> Result<(), String> {
    for op in ops {
        match op {
            AiTypedOp::Cells(items) => {
                execute_ai_ops(&mut st.model, items)?;
                for item in items {
                    let (sheet, row, col) = item.target();
                    st.rich_text.remove(&(sheet, row, col));
                    st.rich_text_xml.remove(&(sheet, row, col));
                }
            }
            AiTypedOp::SetFormat {
                sheet,
                range,
                styles,
                ..
            } => {
                let target = area(*sheet, range[0], range[1], range[2], range[3]);
                for (path, value) in styles {
                    st.model.update_range_style(&target, path, value)?;
                }
            }
            AiTypedOp::SetBorder {
                sheet,
                range,
                border_type,
                style,
                color,
                ..
            } => {
                let request = serde_json::to_vec(&json!({
                    "sheet":sheet,"r0":range[0],"c0":range[1],"r1":range[2],"c1":range[3],
                    "type":border_type,"style":style,"color":color,
                }))
                .map_err(|error| error.to_string())?;
                api_border(st, &request)?;
            }
            AiTypedOp::UpdateChart { sheet, id, patch } => {
                ai_update_chart(st, *sheet, id, patch)?;
            }
            AiTypedOp::UpdatePivotTable { part, patch } => {
                let request = serde_json::to_vec(&json!({
                    "op":"update","part":part,"patch":patch,
                }))
                .map_err(|error| error.to_string())?;
                api_pivot_tables(st, &request)?;
            }
        }
    }
    Ok(())
}

fn ai_flatten_cell_ops(ops: &[AiTypedOp]) -> Vec<ai_context::CellOp> {
    ops.iter()
        .filter_map(|op| match op {
            AiTypedOp::Cells(items) => Some(items.as_slice()),
            _ => None,
        })
        .flatten()
        .cloned()
        .collect()
}

fn ai_object_snapshots(
    st: &AppState,
    ops: &[AiTypedOp],
    names: &[String],
) -> Result<std::collections::BTreeMap<String, Value>, String> {
    let mut snapshots = std::collections::BTreeMap::new();
    for op in ops {
        match op {
            AiTypedOp::UpdateChart { sheet, id, .. } => {
                let object = st
                    .objects
                    .get(sheet)
                    .and_then(|objects| {
                        objects.iter().find(|object| {
                            object.get("id").and_then(Value::as_str) == Some(id.as_str())
                        })
                    })
                    .ok_or_else(|| format!("找不到原生图表：{id}"))?;
                snapshots.insert(
                    format!("chart:{sheet}:{id}"),
                    json!({
                        "kind":"chart","sheet":names[*sheet as usize],"id":id,
                        "model":ai_chart_model_from_object(st, object)?,
                    }),
                );
            }
            AiTypedOp::UpdatePivotTable { part, .. } => {
                let model = pivot_table_model_with_edits(st)?;
                let table = model
                    .get("tables")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .find(|table| table.get("part").and_then(Value::as_str) == Some(part.as_str()))
                    .cloned()
                    .ok_or_else(|| format!("找不到原生透视表：{part}"))?;
                snapshots.insert(
                    format!("pivotTable:{part}"),
                    json!({"kind":"pivotTable","part":part,"model":table}),
                );
            }
            _ => {}
        }
    }
    Ok(snapshots)
}

fn ai_object_diff_json(
    before: &std::collections::BTreeMap<String, Value>,
    after: &std::collections::BTreeMap<String, Value>,
) -> Vec<Value> {
    before
        .iter()
        .filter_map(|(key, old)| {
            let new = after.get(key)?;
            (old != new).then(|| {
                json!({
                    "target":key,
                    "kind":old.get("kind").cloned().unwrap_or(Value::Null),
                    "before":old,
                    "after":new,
                })
            })
        })
        .collect()
}

fn ai_operation_summaries(ops: &[AiTypedOp], names: &[String]) -> Vec<Value> {
    ops.iter()
        .map(|op| match op {
            AiTypedOp::Cells(items) => json!({"kind":"cells","expandedCount":items.len()}),
            AiTypedOp::SetFormat {
                reference, styles, ..
            } => json!({"kind":"format","ref":reference,"fields":styles.iter().map(|(path, _)| path).collect::<Vec<_>>()}),
            AiTypedOp::SetBorder {
                reference,
                border_type,
                style,
                color,
                ..
            } => json!({"kind":"border","ref":reference,"type":border_type,"style":style,"color":color}),
            AiTypedOp::UpdateChart { sheet, id, .. } => {
                json!({"kind":"chart","sheet":names[*sheet as usize],"id":id})
            }
            AiTypedOp::UpdatePivotTable { part, .. } => {
                json!({"kind":"pivotTable","part":part})
            }
        })
        .collect()
}

fn ai_validation_candidate(op: &ai_context::CellOp) -> Value {
    match op {
        ai_context::CellOp::SetValue { value, .. } => {
            json!({ "kind": "userInput", "value": value })
        }
        ai_context::CellOp::SetFormula { formula, .. } => {
            json!({ "kind": "formula", "value": formula })
        }
        ai_context::CellOp::Clear { .. } => json!({ "kind": "blank" }),
    }
}

fn ai_validation_violations(
    model: &UserModel<'_>,
    features: &WorksheetFeatureTransport,
    ops: &[ai_context::CellOp],
    names: &[String],
) -> Vec<Value> {
    let mut final_ops = std::collections::BTreeMap::new();
    for op in ops {
        final_ops.insert(op.target(), op);
    }
    let mut out = Vec::new();
    for ((sheet, row, col), op) in final_ops {
        let Some(transport) = features.data_validations.get(&sheet) else {
            continue;
        };
        for rule in transport
            .rules
            .iter()
            .filter(|rule| data_validation_sqref_contains(&rule.sqref, row, col))
        {
            let request = json!({
                "row": row,
                "col": col,
                "rule": rule,
                "candidate": ai_validation_candidate(op),
            });
            let cell_ref = ai_context::qualify(&names[sheet as usize], row, col, row, col);
            match validate_data_validation_candidate_with_model(model, features, sheet, &request) {
                Ok(mut outcome) if outcome.get("valid").and_then(Value::as_bool) == Some(false) => {
                    outcome["ref"] = json!(cell_ref);
                    outcome["ruleId"] = json!(rule.id);
                    out.push(outcome);
                }
                Ok(_) => {}
                Err(error) => out.push(json!({
                    "ref": cell_ref,
                    "ruleId": rule.id,
                    "valid": false,
                    "reason": "evaluationError",
                    "error": error,
                })),
            }
        }
    }
    out
}

fn ai_scan_touched_errors(
    model: &UserModel<'_>,
    ops: &[ai_context::CellOp],
    names: &[String],
) -> Result<Vec<Value>, String> {
    let sheets = ops
        .iter()
        .map(|op| op.target().0)
        .collect::<std::collections::BTreeSet<_>>();
    let mut errors = Vec::new();
    for sheet in sheets {
        errors.extend(ai_context::scan_errors(
            model,
            sheet,
            &names[sheet as usize],
            200usize.saturating_sub(errors.len()),
        )?);
        if errors.len() >= 200 {
            break;
        }
    }
    Ok(errors)
}

fn api_ai_apply(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = parse_body(body)?;
    let dry_run = request
        .get("dryRun")
        .map(|value| value.as_bool().ok_or("dryRun 必须是布尔值"))
        .transpose()?
        .unwrap_or(true);
    let items = request
        .get("ops")
        .and_then(Value::as_array)
        .ok_or("缺少 ops 数组")?;
    if items.is_empty() {
        return Err("ops 不能为空".into());
    }
    let names = st.model.get_model().workbook.get_worksheet_names();
    let selected_sheet = st.model.get_selected_view().sheet;
    let default_sheet = ai_sheet_index(&names, request.get("sheet"), selected_sheet)?;

    // 完整解析和表名解析必须发生在任何写入之前，保证坏请求零写入。
    let ops = ai_parse_typed_ops(st, items, default_sheet, &names)?;
    let cell_ops = ai_flatten_cell_ops(&ops);
    if cell_ops.len() as i64 > ai_context::MAX_DIFF_CELLS {
        return Err(format!(
            "展开后的操作数 {} 超过上限 {}，请拆成多次请求",
            cell_ops.len(),
            ai_context::MAX_DIFF_CELLS
        ));
    }
    let (scope, diff_truncated) = ai_diff_scope(&st.model, &ops)?;
    let before = ai_snapshot(st, &scope)?;
    let before_objects = ai_object_snapshots(st, &ops, &names)?;

    let (diff, object_diff, errors, validation_violations) = if dry_run {
        let mut sandbox = ai_preview_state(st)?;
        execute_ai_typed_ops(&mut sandbox, &ops)?;
        let after = ai_snapshot(&sandbox, &scope)?;
        let after_objects = ai_object_snapshots(&sandbox, &ops, &names)?;
        (
            ai_diff_json(&before, &after, &names),
            ai_object_diff_json(&before_objects, &after_objects),
            ai_scan_touched_errors(&sandbox.model, &cell_ops, &names)?,
            ai_validation_violations(
                &sandbox.model,
                &sandbox.worksheet_features,
                &cell_ops,
                &names,
            ),
        )
    } else {
        execute_ai_typed_ops(st, &ops)?;
        let after = ai_snapshot(st, &scope)?;
        let after_objects = ai_object_snapshots(st, &ops, &names)?;
        (
            ai_diff_json(&before, &after, &names),
            ai_object_diff_json(&before_objects, &after_objects),
            ai_scan_touched_errors(&st.model, &cell_ops, &names)?,
            ai_validation_violations(&st.model, &st.worksheet_features, &cell_ops, &names),
        )
    };

    ok_json(json!({
        "dryRun": dry_run,
        "applied": !dry_run,
        "operationCount": items.len(),
        "expandedCellOperationCount": cell_ops.len(),
        "operations": ai_operation_summaries(&ops, &names),
        "changedCells": diff.len(),
        "changedObjects": object_diff.len(),
        "diff": diff,
        "objectDiff": object_diff,
        "diffTruncated": diff_truncated,
        "errors": errors,
        "validationViolations": validation_violations,
    }))
}

fn api_input(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let ranges = merged_ranges(st, sheet)?;
    let (row, col) = merged_anchor_in(&ranges, ji(&v, "row")? as i32, ji(&v, "col")? as i32);
    let value = js(&v, "value")?;
    let key = (sheet, row, col);
    // Opening a rich-text cell in Excel's plain formula editor and committing without changing
    // any characters must be a true no-op.  Calling set_user_input here would flatten the shared
    // string and the two removals below would silently destroy every per-run style.
    if (st.rich_text.contains_key(&key) || st.rich_text_xml.contains_key(&key))
        && st.model.get_cell_content(sheet, row, col)? == value
    {
        let formatted = st.model.get_formatted_cell_value(sheet, row, col)?;
        return ok_json(json!({ "v": formatted, "richTextPreserved": true, "unchanged": true }));
    }
    st.model.set_user_input(sheet, row, col, value)?;
    st.rich_text.remove(&key);
    st.rich_text_xml.remove(&key);
    auto_expand_table_for_input(st, sheet, row, col)?;
    // 回传计算结果：前端据此提示循环引用（#CIRC!）等错误
    let formatted = st.model.get_formatted_cell_value(sheet, row, col)?;
    ok_json(json!({ "v": formatted }))
}

// Ctrl+Enter：把输入写入整个选区，公式按相对引用随锚点平移（Excel 语义）
fn api_inputrange(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let (r0, c0, r1, c1) = clamp_range(
        ji(&v, "r0")? as i32,
        ji(&v, "c0")? as i32,
        ji(&v, "r1")? as i32,
        ji(&v, "c1")? as i32,
    );
    let ranges = merged_ranges(st, sheet)?;
    let (row, col) = merged_anchor_in(&ranges, ji(&v, "row")? as i32, ji(&v, "col")? as i32); // 锚点：编辑发生的逻辑单元格（合并区恒为左上角）
    let value = js(&v, "value")?;
    if (r1 - r0 + 1) as i64 * (c1 - c0 + 1) as i64 > 200_000 {
        return Err("range too large".into());
    }
    st.model.pause_evaluation();
    let mut result = Ok(());
    let mut visited = std::collections::HashSet::new();
    let mut table_expansion_cells = Vec::new();
    'outer: for r in r0..=r1 {
        for c in c0..=c1 {
            let (r, c) = merged_anchor_in(&ranges, r, c);
            if !visited.insert((r, c)) {
                continue;
            }
            let val = if value.starts_with('=') {
                shift_formula_refs(value, r - row, c - col)
            } else {
                value.to_string()
            };
            let key = (sheet, r, c);
            if (st.rich_text.contains_key(&key) || st.rich_text_xml.contains_key(&key))
                && st.model.get_cell_content(sheet, r, c)? == val
            {
                continue;
            }
            if let Err(e) = st.model.set_user_input(sheet, r, c, &val) {
                result = Err(e);
                break 'outer;
            }
            table_expansion_cells.push((r, c));
            st.rich_text.remove(&key);
            st.rich_text_xml.remove(&key);
        }
    }
    st.model.resume_evaluation();
    st.model.evaluate();
    result?;
    auto_expand_tables_for_cells(st, sheet, &table_expansion_cells)?;
    ok_json(json!({}))
}

fn api_batch(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let items = v
        .get("cells")
        .and_then(|x| x.as_array())
        .ok_or("missing cells")?;
    let ranges = merged_ranges(st, sheet)?;
    let mut resolved = Vec::with_capacity(items.len());
    let mut visited = std::collections::HashSet::new();
    for it in items {
        let (row, col) = merged_anchor_in(&ranges, ji(it, "r")? as i32, ji(it, "c")? as i32);
        if !visited.insert((row, col)) {
            return Err("multiple pasted cells target the same merged cell".into());
        }
        resolved.push((row, col, js(it, "v")?.to_string()));
    }
    st.model.pause_evaluation();
    let mut result = Ok(());
    for (row, col, val) in &resolved {
        let key = (sheet, *row, *col);
        if (st.rich_text.contains_key(&key) || st.rich_text_xml.contains_key(&key))
            && st.model.get_cell_content(sheet, *row, *col)? == *val
        {
            continue;
        }
        if let Err(e) = st.model.set_user_input(sheet, *row, *col, val) {
            result = Err(e);
            break;
        }
        st.rich_text.remove(&key);
        st.rich_text_xml.remove(&key);
    }
    st.model.resume_evaluation();
    st.model.evaluate();
    result?;
    let expansion_cells = resolved
        .iter()
        .map(|(row, col, _)| (*row, *col))
        .collect::<Vec<_>>();
    auto_expand_tables_for_cells(st, sheet, &expansion_cells)?;
    ok_json(json!({}))
}

fn api_style(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let rng = area(
        sheet,
        ji(&v, "r0")? as i32,
        ji(&v, "c0")? as i32,
        ji(&v, "r1")? as i32,
        ji(&v, "c1")? as i32,
    );
    let path = js(&v, "path")?;
    let value = js(&v, "value")?;
    st.model.update_range_style(&rng, path, value)?;
    ok_json(json!({}))
}

// 边框：Excel 边框菜单语义（所有框线/外框/内框/上下左右/无）
fn api_border(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let rng = area(
        sheet,
        ji(&v, "r0")? as i32,
        ji(&v, "c0")? as i32,
        ji(&v, "r1")? as i32,
        ji(&v, "c1")? as i32,
    );
    let btype = match js(&v, "type")? {
        "all" => "All",
        "inner" => "Inner",
        "outer" => "Outer",
        "top" => "Top",
        "right" => "Right",
        "bottom" => "Bottom",
        "left" => "Left",
        "centerh" => "CenterH",
        "centerv" => "CenterV",
        "none" => "None",
        other => return Err(format!("bad border type: {other}")),
    };
    let bstyle = v.get("style").and_then(|x| x.as_str()).unwrap_or("thin");
    let color = v.get("color").and_then(|x| x.as_str()).unwrap_or("#000000");
    // BorderArea 字段非公开，经 serde 构造（官方推荐的跨层传递方式）
    let ba: BorderArea = serde_json::from_value(json!({
        "item": { "style": bstyle, "color": color },
        "type": btype,
    }))
    .map_err(|e| format!("border area: {e}"))?;
    st.model.set_area_with_border(&rng, &ba)?;
    ok_json(json!({}))
}

// 插入对象 CRUD：list(GET) / add / update / delete(POST)
fn api_objects(st: &mut AppState, query: &str, body: &[u8]) -> Result<Resp, String> {
    if body.is_empty() {
        // GET list
        let sheet = qi(query, "sheet", 0) as u32;
        let objs = st.objects.get(&sheet).cloned().unwrap_or_default();
        return ok_json(json!({ "objects": objs }));
    }
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let objs = st.objects.entry(sheet).or_default();
    match js(&v, "op")? {
        "add" => {
            let obj = v.get("object").cloned().ok_or("missing object")?;
            objs.push(obj);
        }
        "update" => {
            let id = js(&v, "id")?;
            let obj = v.get("object").cloned().ok_or("missing object")?;
            if let Some(slot) = objs.iter_mut().find(|x| x["id"].as_str() == Some(id)) {
                *slot = obj;
            }
        }
        "delete" => {
            let id = js(&v, "id")?;
            objs.retain(|x| x["id"].as_str() != Some(id));
        }
        other => return Err(format!("bad objects op: {other}")),
    }
    ok_json(json!({ "count": objs.len() }))
}

fn api_native_drawing_validate(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = parse_body(body)?;
    let descriptor = request
        .get("nativeDrawing")
        .or_else(|| {
            request
                .get("object")
                .and_then(|object| object.get("config"))
                .and_then(|config| config.get("nativeDrawing"))
        })
        .ok_or("missing nativeDrawing")?;
    let snapshot = st
        .source_ooxml
        .as_ref()
        .ok_or("native DrawingML source is unavailable")?;
    let kind = descriptor
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("native drawing kind is missing")?;
    let model = match kind {
        "chart" | "smartart" => {
            let part = descriptor
                .get("contentPart")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("native {kind} content part is missing"))?;
            let xml = snapshot
                .parts
                .get(part)
                .ok_or_else(|| format!("native {kind} content part {part} is unavailable"))?;
            let xml = std::str::from_utf8(xml)
                .map_err(|error| format!("native {kind} content utf8: {error}"))?;
            if kind == "chart" {
                let updated = if let Some(edit) = native_descriptor_edit(descriptor, "chart") {
                    native_chart_edit::apply_chart_edit(xml, edit)?
                } else {
                    xml.to_string()
                };
                native_chart_edit::parse_chart_model(&updated)
            } else {
                let updated = if let Some(edit) = native_descriptor_edit(descriptor, "smartart") {
                    native_smartart_edit::apply_smartart_edit(xml, edit)?
                } else {
                    xml.to_string()
                };
                native_smartart_edit::parse_smartart_model(&updated)
            }
        }
        "shape" | "connector" | "group" => {
            let drawing_path = descriptor
                .get("drawingPath")
                .and_then(Value::as_str)
                .ok_or("native shape drawing path is missing")?;
            let drawing = snapshot
                .parts
                .get(drawing_path)
                .ok_or_else(|| format!("native drawing {drawing_path} is unavailable"))?;
            let drawing = std::str::from_utf8(drawing)
                .map_err(|error| format!("native drawing utf8: {error}"))?;
            let anchors = drawing_anchor_slices(drawing);
            let wanted_id = descriptor
                .get("nonVisualId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let wanted_index = descriptor
                .get("anchorIndex")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX) as usize;
            let found = anchors
                .iter()
                .enumerate()
                .find(|(index, (start, end, _))| {
                    let raw = &drawing[*start..*end];
                    (!wanted_id.is_empty()
                        && drawing_any_element_attr(raw, "cNvPr", "id").as_deref()
                            == Some(wanted_id))
                        || (wanted_id.is_empty() && *index == wanted_index)
                })
                .or_else(|| {
                    anchors
                        .get(wanted_index)
                        .map(|anchor| (wanted_index, anchor))
                });
            let Some((_, (start, end, _))) = found else {
                return Err("native shape anchor is unavailable".to_string());
            };
            let raw = &drawing[*start..*end];
            let updated = if let Some(edit) = native_descriptor_edit(descriptor, "shape") {
                apply_native_shape_anchor_edit(raw, drawing, edit)?
            } else {
                raw.to_string()
            };
            native_shape_edit::parse_shape_model(&drawing_fragment_document_with_source(
                &updated,
                Some(drawing),
            ))
        }
        _ => return Err(format!("native {kind} content is not editable")),
    };
    if let Some(error) = model.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    ok_json(json!({"kind":kind,"model":model}))
}

// 合并/取消合并单元格（引擎 vendor 分支新增 API，Excel 语义：保留左上角内容）
fn api_merge(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let (r0, c0, r1, c1) = clamp_range(
        ji(&v, "r0")? as i32,
        ji(&v, "c0")? as i32,
        ji(&v, "r1")? as i32,
        ji(&v, "c1")? as i32,
    );
    match js(&v, "op")? {
        "merge" => st.model.merge_cells_range(sheet, r0, c0, r1, c1)?,
        "unmerge" => st.model.unmerge_cells_range(sheet, r0, c0, r1, c1)?,
        other => return Err(format!("bad merge op: {other}")),
    }
    ok_json(json!({ "merges": merges_json(st, sheet)? }))
}

// 条件格式规则管理器：一次 API 请求是一个应用级 undo/redo 事务；元数据更新不会重建 DXF。
fn api_cf(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let outcome = cf_manager::execute(&mut st.model, sheet, &v)?;
    if outcome.has_rules {
        st.cf_sheets.insert(sheet);
    } else {
        st.cf_sheets.remove(&sheet);
    }
    ok_json(outcome.response)
}

// Excel 数据验证：标准 dataValidation 规则使用 typed transport 编辑；未知属性、子节点和
// worksheet extLst 中的 x14 验证器仍由 OOXML 差量层原样保留。
fn data_validation_ref_cell(reference: &str) -> Option<(i32, i32)> {
    let reference = reference
        .rsplit_once('!')
        .map(|(_, local)| local)
        .unwrap_or(reference)
        .split(':')
        .next()?
        .replace('$', "");
    parse_a1(&reference)
}

fn data_validation_sqref_contains(sqref: &str, row: i32, col: i32) -> bool {
    sqref.split_whitespace().any(|area| {
        let mut bounds = area.split(':');
        let Some((r0, c0)) = bounds.next().and_then(data_validation_ref_cell) else {
            return false;
        };
        let (r1, c1) = bounds
            .next()
            .and_then(data_validation_ref_cell)
            .unwrap_or((r0, c0));
        row >= r0.min(r1) && row <= r0.max(r1) && col >= c0.min(c1) && col <= c0.max(c1)
    })
}

fn validation_candidate_json(value: &Value) -> Result<Value, String> {
    if let Some(candidate) = value.get("candidate") {
        return Ok(candidate.clone());
    }
    let value = value.get("value").unwrap_or(&Value::Null);
    Ok(match value {
        Value::Null => json!({"kind":"blank"}),
        Value::Bool(value) => json!({"kind":"boolean","value":value}),
        Value::Number(value) => json!({"kind":"number","value":value}),
        Value::String(value) if value.starts_with('=') => {
            json!({"kind":"formula","value":value})
        }
        Value::String(value) => json!({"kind":"userInput","value":value}),
        _ => return Err("data validation candidate must be scalar".into()),
    })
}

fn validate_data_validation_candidate(
    st: &AppState,
    sheet: u32,
    request: &Value,
) -> Result<Value, String> {
    validate_data_validation_candidate_with_model(&st.model, &st.worksheet_features, sheet, request)
}

fn validate_data_validation_candidate_with_model(
    model: &UserModel<'_>,
    worksheet_features: &WorksheetFeatureTransport,
    sheet: u32,
    request: &Value,
) -> Result<Value, String> {
    let row = ji(request, "row")? as i32;
    let col = request
        .get("col")
        .or_else(|| request.get("column"))
        .and_then(Value::as_i64)
        .ok_or("missing column")? as i32;
    let rule = if let Some(rule) = request.get("rule") {
        serde_json::from_value::<DataValidationRule>(rule.clone())
            .map_err(|error| format!("bad data validation rule: {error}"))?
    } else {
        let id = request.get("id").and_then(Value::as_str);
        worksheet_features
            .data_validations
            .get(&sheet)
            .and_then(|transport| {
                transport.rules.iter().find(|rule| {
                    id.map(|id| rule.id == id)
                        .unwrap_or_else(|| data_validation_sqref_contains(&rule.sqref, row, col))
                })
            })
            .cloned()
            .ok_or("data validation rule is unavailable for the target cell")?
    };
    let (anchor_row, anchor_col) = rule
        .sqref
        .split_whitespace()
        .next()
        .and_then(data_validation_ref_cell)
        .ok_or("data validation sqref has no valid anchor")?;
    let validation_type = match rule.validation_type.as_str() {
        "none" | "any" => "any",
        value => value,
    };
    let normalized = json!({
        "rule":{
            "validationType":validation_type,
            "operator":rule.operator,
            "allowBlank":rule.allow_blank,
            "formula1":rule.formula1,
            "formula2":rule.formula2,
            "anchor":{"sheet":sheet,"row":anchor_row,"column":anchor_col}
        },
        "target":{"sheet":sheet,"row":row,"column":col},
        "candidate":validation_candidate_json(request)?
    });
    let runtime_request: validation_runtime::ValidationRequest = serde_json::from_value(normalized)
        .map_err(|error| format!("bad validation runtime request: {error}"))?;
    serde_json::to_value(validation_runtime::validate_candidate(
        model,
        &runtime_request,
    )?)
    .map_err(|error| error.to_string())
}

fn api_data_validation(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    if sheet as usize >= st.model.get_model().workbook.worksheets.len() {
        return Err(format!("invalid sheet index {sheet}"));
    }
    match js(&v, "op")? {
        "validate" => ok_json(validate_data_validation_candidate(st, sheet, &v)?),
        "list" => {
            let rules = st
                .worksheet_features
                .data_validations
                .get(&sheet)
                .map(|transport| transport.rules.clone())
                .unwrap_or_default();
            ok_json(json!({ "rules": rules }))
        }
        "add" => {
            let mut rule: DataValidationRule = serde_json::from_value(
                v.get("rule")
                    .cloned()
                    .ok_or("missing data validation rule")?,
            )
            .map_err(|error| format!("bad data validation rule: {error}"))?;
            rule.id = format!(
                "dv-new-{}",
                DATA_VALIDATION_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            rule.raw_xml = None;
            let rule = normalize_data_validation_rule(rule)?;
            let response = rule.clone();
            st.worksheet_features
                .data_validations
                .entry(sheet)
                .or_default()
                .rules
                .push(rule);
            st.worksheet_features.data_validation_dirty.insert(sheet);
            ok_json(json!({ "rule": response }))
        }
        "update" => {
            let id = js(&v, "id")?.to_string();
            let mut replacement: DataValidationRule = serde_json::from_value(
                v.get("rule")
                    .cloned()
                    .ok_or("missing data validation rule")?,
            )
            .map_err(|error| format!("bad data validation rule: {error}"))?;
            let transport = st
                .worksheet_features
                .data_validations
                .get_mut(&sheet)
                .ok_or("data validation rule is unavailable")?;
            let current = transport
                .rules
                .iter_mut()
                .find(|rule| rule.id == id)
                .ok_or("data validation rule is unavailable")?;
            replacement.id = current.id.clone();
            replacement.raw_xml = current.raw_xml.clone();
            replacement = normalize_data_validation_rule(replacement)?;
            *current = replacement.clone();
            st.worksheet_features.data_validation_dirty.insert(sheet);
            ok_json(json!({ "rule": replacement }))
        }
        "delete" => {
            let id = js(&v, "id")?;
            let transport = st
                .worksheet_features
                .data_validations
                .get_mut(&sheet)
                .ok_or("data validation rule is unavailable")?;
            let before = transport.rules.len();
            transport.rules.retain(|rule| rule.id != id);
            if transport.rules.len() == before {
                return Err("data validation rule is unavailable".to_string());
            }
            st.worksheet_features.data_validation_dirty.insert(sheet);
            ok_json(json!({}))
        }
        "clear" => {
            st.worksheet_features
                .data_validations
                .entry(sheet)
                .or_default()
                .rules
                .clear();
            st.worksheet_features.data_validation_dirty.insert(sheet);
            ok_json(json!({}))
        }
        other => Err(format!("bad data validation op: {other}")),
    }
}

fn what_if_payload<'a>(request: &'a Value) -> &'a Value {
    request.get("request").unwrap_or(request)
}

fn apply_what_if_writes(
    st: &mut AppState,
    request: &Value,
    writes: &[what_if_runtime::CellWrite],
) -> Result<(), String> {
    if writes.is_empty() {
        return Err("what-if operation produced no cell writes".into());
    }
    let mut sheets = std::collections::BTreeMap::<u32, Vec<Value>>::new();
    for write in writes {
        sheets.entry(write.cell.sheet).or_default().push(json!({
            "r": write.cell.row,
            "c": write.cell.column,
            "v": write.input,
        }));
    }
    for (sheet, cells) in sheets {
        let mut batch = json!({"sheet":sheet,"cells":cells});
        if let Some(password) = request.get("protectionPassword").and_then(Value::as_str) {
            batch["protectionPassword"] = Value::String(password.to_string());
        }
        let bytes = serde_json::to_vec(&batch)
            .map_err(|error| format!("what-if write encoding failed: {error}"))?;
        // `/api/what-if` has a nested request shape. Enforce the concrete output cells through the
        // regular batch-edit protection path, then reuse batch input so merged-cell anchoring,
        // rich-text invalidation and structured-table expansion remain identical to paste.
        enforce_runtime_protection(st, "/api/batch", &bytes)?;
        api_batch(st, &bytes)?;
    }
    Ok(())
}

fn parse_what_if_request<T: serde::de::DeserializeOwned>(request: &Value) -> Result<T, String> {
    serde_json::from_value(what_if_payload(request).clone())
        .map_err(|error| format!("bad what-if request: {error}"))
}

fn api_what_if(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = parse_body(body)?;
    let op = request.get("op").and_then(Value::as_str).unwrap_or("list");
    let kind = request
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("scenarios");
    match (op, kind) {
        ("preview", "goalSeek") => {
            let goal: what_if_runtime::GoalSeekRequest = parse_what_if_request(&request)?;
            ok_json(
                serde_json::to_value(what_if_runtime::goal_seek(&st.model, &goal)?)
                    .map_err(|error| error.to_string())?,
            )
        }
        ("apply", "goalSeek") => {
            let goal: what_if_runtime::GoalSeekRequest = parse_what_if_request(&request)?;
            let outcome = what_if_runtime::goal_seek(&st.model, &goal)?;
            if !outcome.converged {
                return Err(format!("Goal Seek did not converge: {}", outcome.message));
            }
            let write = outcome
                .write
                .clone()
                .ok_or("converged Goal Seek result has no changing-cell write")?;
            apply_what_if_writes(st, &request, &[write])?;
            ok_json(serde_json::to_value(outcome).map_err(|error| error.to_string())?)
        }
        ("preview", "dataTable") => {
            let table: what_if_runtime::DataTableRequest = parse_what_if_request(&request)?;
            ok_json(
                serde_json::to_value(what_if_runtime::data_table(&st.model, &table)?)
                    .map_err(|error| error.to_string())?,
            )
        }
        ("apply", "dataTable") => {
            let table: what_if_runtime::DataTableRequest = parse_what_if_request(&request)?;
            let outcome = what_if_runtime::data_table(&st.model, &table)?;
            apply_what_if_writes(st, &request, &outcome.writes)?;
            ok_json(serde_json::to_value(outcome).map_err(|error| error.to_string())?)
        }
        ("list", "scenarios") => ok_json(json!({
            "scenarios": st.what_if_scenarios.list(),
            "maxChangingCells": 32,
        })),
        ("create", "scenario") => {
            let scenario: what_if_runtime::Scenario =
                serde_json::from_value(request.get("scenario").cloned().ok_or("missing scenario")?)
                    .map_err(|error| format!("bad scenario: {error}"))?;
            let scenario = st.what_if_scenarios.create(scenario)?;
            ok_json(json!({"scenario":scenario}))
        }
        ("update", "scenario") => {
            let id = request
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing scenario id")?;
            let scenario: what_if_runtime::Scenario =
                serde_json::from_value(request.get("scenario").cloned().ok_or("missing scenario")?)
                    .map_err(|error| format!("bad scenario: {error}"))?;
            let scenario = st.what_if_scenarios.update(id, scenario)?;
            ok_json(json!({"scenario":scenario}))
        }
        ("delete", "scenario") => {
            let id = request
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing scenario id")?;
            let scenario = st.what_if_scenarios.delete(id)?;
            ok_json(json!({"scenario":scenario}))
        }
        ("preview", "scenario") | ("apply", "scenario") => {
            let id = request
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing scenario id")?;
            let scenario = st
                .what_if_scenarios
                .get(id)
                .cloned()
                .ok_or_else(|| format!("scenario not found: {id}"))?;
            let preview = what_if_runtime::preview_scenario(&st.model, &scenario)?;
            if op == "apply" {
                apply_what_if_writes(st, &request, &preview.writes)?;
            }
            ok_json(serde_json::to_value(preview).map_err(|error| error.to_string())?)
        }
        _ => Err(format!("unsupported what-if operation: {op}/{kind}")),
    }
}

fn pivot_refresh_patch_fields(value: &Value) -> Result<serde_json::Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or("pivot cache patch must be an object")?;
    if let Some(refresh) = object.get("refresh") {
        if object.len() != 1 {
            return Err("pivot cache refresh wrapper cannot be mixed with direct fields".into());
        }
        refresh
            .as_object()
            .cloned()
            .ok_or_else(|| "pivot cache refresh must be an object".to_string())
    } else {
        Ok(object.clone())
    }
}

fn pivot_cache_model_with_edits(st: &AppState) -> Result<Value, String> {
    let Some(snapshot) = st.source_ooxml.as_ref() else {
        return Ok(json!({"workbookPart":null,"caches":[]}));
    };
    let mut model = native_pivot_cache_edit::parse_pivot_cache_model(&snapshot.parts)?;
    if let Some(caches) = model.get_mut("caches").and_then(Value::as_array_mut) {
        for cache in caches {
            let Some(part) = cache.get("part").and_then(Value::as_str) else {
                continue;
            };
            let Some(patch) = st.pivot_cache_refresh_edits.get(part) else {
                continue;
            };
            let fields = pivot_refresh_patch_fields(patch)?;
            let Some(refresh) = cache.get_mut("refresh").and_then(Value::as_object_mut) else {
                continue;
            };
            for (name, value) in fields {
                refresh.insert(name, value);
            }
            cache
                .as_object_mut()
                .map(|object| object.insert("edited".to_string(), Value::Bool(true)));
        }
    }
    Ok(model)
}

// Native PivotCache refresh policy editor.  It intentionally does not regenerate fields,
// records, PivotTables, slicers, or timelines; only six root attributes are patched in place.
fn api_pivot_caches(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = if body.is_empty() {
        json!({"op":"list"})
    } else {
        parse_body(body)?
    };
    match request.get("op").and_then(Value::as_str).unwrap_or("list") {
        "list" => ok_json(pivot_cache_model_with_edits(st)?),
        "update" => {
            let snapshot = st
                .source_ooxml
                .as_ref()
                .ok_or("workbook has no native PivotCache")?;
            let cache_id = request
                .get("cacheId")
                .and_then(Value::as_u64)
                .ok_or("missing pivot cacheId")?;
            let part = request
                .get("part")
                .and_then(Value::as_str)
                .ok_or("missing pivot cache part")?;
            let patch = request.get("patch").ok_or("missing pivot cache patch")?;
            let model = native_pivot_cache_edit::parse_pivot_cache_model(&snapshot.parts)?;
            let exists = model["caches"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|cache| {
                    cache["cacheId"].as_u64() == Some(cache_id)
                        && cache["part"].as_str() == Some(part)
                });
            if !exists {
                return Err("pivot cache stable key is unavailable".to_string());
            }
            let original = snapshot
                .parts
                .get(part)
                .ok_or("pivot cache part is unavailable")?;
            let original = std::str::from_utf8(original)
                .map_err(|error| format!("pivot cache UTF-8: {error}"))?;
            let current = if let Some(existing) = st.pivot_cache_refresh_edits.get(part) {
                native_pivot_cache_edit::apply_pivot_cache_refresh_patch(original, existing)?
            } else {
                original.to_string()
            };
            native_pivot_cache_edit::apply_pivot_cache_refresh_patch(&current, patch)?;
            let mut merged = st
                .pivot_cache_refresh_edits
                .get(part)
                .map(pivot_refresh_patch_fields)
                .transpose()?
                .unwrap_or_default();
            merged.extend(pivot_refresh_patch_fields(patch)?);
            st.pivot_cache_refresh_edits
                .insert(part.to_string(), Value::Object(merged));
            ok_json(pivot_cache_model_with_edits(st)?)
        }
        "reset" => {
            let part = request
                .get("part")
                .and_then(Value::as_str)
                .ok_or("missing pivot cache part")?;
            st.pivot_cache_refresh_edits.remove(part);
            ok_json(pivot_cache_model_with_edits(st)?)
        }
        other => Err(format!("bad pivot cache op: {other}")),
    }
}

fn apply_pivot_table_edit_journal(
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    edits: &[Value],
) -> Result<(), String> {
    for edit in edits {
        let part = edit
            .get("part")
            .and_then(Value::as_str)
            .ok_or("stored PivotTable edit has no part")?;
        let patch = edit
            .get("patch")
            .ok_or("stored PivotTable edit has no patch")?;
        let original = parts
            .get(part)
            .ok_or_else(|| format!("edited PivotTable part {part} is unavailable"))?;
        let original = std::str::from_utf8(original)
            .map_err(|error| format!("edited PivotTable UTF-8: {error}"))?;
        let edited = native_pivot_table_edit::apply_pivot_table_patch(original, patch)?;
        parts.insert(part.to_string(), edited.into_bytes());
    }
    Ok(())
}

fn apply_pivot_local_refresh_journal(
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    edits: &[Value],
) -> Result<(), String> {
    for edit in edits {
        let request = edit
            .get("request")
            .ok_or("stored local Pivot refresh has no request")?;
        let result = edit
            .get("result")
            .ok_or("stored local Pivot refresh has no result")?;
        pivot_local_refresh::apply_local_refresh_ooxml(parts, request, result)?;
    }
    Ok(())
}

fn apply_slicer_edit_journal(
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    edits: &[Value],
) -> Result<(), String> {
    for edit in edits {
        native_slicer_edit::apply_slicer_package_edit(parts, edit)?;
    }
    Ok(())
}

fn apply_native_data_edit_journal(
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    edits: &[Value],
) -> Result<(), String> {
    for edit in edits {
        native_data_runtime::apply_native_data_edit(parts, edit)?;
    }
    Ok(())
}

fn apply_table_edit_journal(
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    edits: &[Value],
) -> Result<(), String> {
    for edit in edits {
        native_table_edit::apply_table_package_edit(parts, edit)?;
    }
    Ok(())
}

fn apply_page_review_edit_journal(
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    edits: &[Value],
) -> Result<(), String> {
    for edit in edits {
        native_page_review_edit::apply_page_review_package_edit(parts, edit)?;
    }
    Ok(())
}

fn materialize_table_feature_parts(
    st: &AppState,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let mut parts = if let Some(snapshot) = st.source_ooxml.as_ref() {
        snapshot.parts.clone()
    } else {
        snapshot_opc_package(&model_to_xlsx_bytes(st)?)?.parts
    };
    apply_native_data_edit_journal(&mut parts, &st.native_data_edits)?;
    apply_table_edit_journal(&mut parts, &st.native_table_edits)?;
    Ok(parts)
}

fn table_model_with_edits(st: &AppState) -> Result<Value, String> {
    native_table_edit::parse_native_table_model(&materialize_table_feature_parts(st)?)
}

fn materialize_native_data_parts(
    st: &AppState,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let mut parts = if let Some(snapshot) = st.source_ooxml.as_ref() {
        snapshot.parts.clone()
    } else {
        snapshot_opc_package(&model_to_xlsx_bytes(st)?)?.parts
    };
    apply_native_data_edit_journal(&mut parts, &st.native_data_edits)?;
    // A later explicit table edit owns the final table range/name, matching Excel's command order.
    apply_table_edit_journal(&mut parts, &st.native_table_edits)?;
    Ok(parts)
}

fn native_data_model_with_edits(st: &AppState) -> Result<Value, String> {
    native_data_runtime::inspect_native_data(&materialize_native_data_parts(st)?)
}

fn api_native_data(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = if body.is_empty() {
        json!({"op":"list"})
    } else {
        parse_body(body)?
    };
    let op = request
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or(if body.is_empty() { "list" } else { "update" });
    match op {
        "list" | "get" | "inspect" => ok_json(native_data_model_with_edits(st)?),
        "update" | "apply" => {
            let patch = request.get("patch").unwrap_or(&request);
            let mut parts = materialize_native_data_parts(st)?;
            let before = parts.clone();
            native_data_runtime::apply_native_data_edit(&mut parts, patch)?;
            if parts != before {
                st.native_data_edits.push(patch.clone());
            }
            ok_json(native_data_model_with_edits(st)?)
        }
        "reset" => {
            st.native_data_edits.clear();
            ok_json(native_data_model_with_edits(st)?)
        }
        other => Err(format!("bad native data op: {other}")),
    }
}

fn api_power_query_execute(body: &[u8]) -> Result<Resp, String> {
    let request = parse_body(body)?;
    ok_json(native_data_runtime::execute_m_subset(&request)?)
}

fn ironcalc_tables_from_native(
    model: &Value,
) -> Result<std::collections::HashMap<String, Table>, String> {
    let mut registry = std::collections::HashMap::new();
    for table in model
        .get("tables")
        .and_then(Value::as_array)
        .ok_or("native table model has no tables array")?
    {
        let name = table
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let display_name = table
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or(&name)
            .to_string();
        let columns = table
            .get("columns")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|column| TableColumn {
                id: column.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                name: column
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Column")
                    .to_string(),
                totals_row_label: column
                    .get("totalsRowLabel")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                header_row_dxf_id: column
                    .get("headerRowDxfId")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                data_dxf_id: column
                    .get("dataDxfId")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                totals_row_dxf_id: column
                    .get("totalsRowDxfId")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                totals_row_function: column
                    .get("totalsRowFunction")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect::<Vec<_>>();
        let style = table.get("styleInfo").unwrap_or(&Value::Null);
        registry.insert(
            display_name.clone(),
            Table {
                name,
                display_name,
                sheet_name: table
                    .get("sheet")
                    .and_then(Value::as_str)
                    .unwrap_or("Sheet1")
                    .to_string(),
                reference: table
                    .get("reference")
                    .and_then(Value::as_str)
                    .unwrap_or("A1:A1")
                    .to_string(),
                totals_row_count: table
                    .get("totalsRowCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                header_row_count: table
                    .get("headerRowCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(1) as u32,
                header_row_dxf_id: table
                    .get("headerRowDxfId")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                data_dxf_id: table
                    .get("dataDxfId")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                totals_row_dxf_id: table
                    .get("totalsRowDxfId")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                columns,
                style_info: TableStyleInfo {
                    name: style
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    show_first_column: style
                        .get("showFirstColumn")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    show_last_column: style
                        .get("showLastColumn")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    show_row_stripes: style
                        .get("showRowStripes")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    show_column_stripes: style
                        .get("showColumnStripes")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
                has_filters: !table.get("autoFilter").is_none_or(Value::is_null),
            },
        );
    }
    Ok(registry)
}

fn explicit_calculated_table_formula(table: &str, formula: &str) -> String {
    let mut result = formula.to_string();
    // OOXML calculatedColumnFormula stores implicit current-table references. IronCalc's parser
    // intentionally accepts the explicit ECMA form, so qualify the common shorthand before use.
    for marker in ["[@[", "[@"] {
        let mut search = 0usize;
        while let Some(relative) = result[search..].find(marker) {
            let start = search + relative;
            let name_start = start + marker.len();
            let suffix = if marker == "[@[" { "]]" } else { "]" };
            let Some(relative_end) = result[name_start..].find(suffix) else {
                break;
            };
            let end = name_start + relative_end;
            let column = result[name_start..end].to_string();
            let replacement = format!("{table}[[#This Row], [{column}]]");
            result.replace_range(start..end + suffix.len(), &replacement);
            search = start + replacement.len();
        }
    }
    result
}

fn explicit_totals_table_formula(table: &str, columns: &[TableColumn], formula: &str) -> String {
    let mut result = formula.to_string();
    for column in columns {
        let shorthand = format!("[{}]", column.name);
        if result.contains(&shorthand) && !result.contains(&format!("{table}{shorthand}")) {
            result = result.replace(&shorthand, &format!("{table}{shorthand}"));
        }
    }
    result
}

fn sync_native_table_cells_scoped(
    st: &mut AppState,
    model: &Value,
    calculated_rows: Option<(i32, i32)>,
    force_calculated: bool,
) -> Result<(), String> {
    let registry = ironcalc_tables_from_native(model)?;
    st.model.replace_tables(registry.clone());
    let sheet_names = st.model.get_model().workbook.get_worksheet_names();
    st.model.pause_evaluation();
    let mut result = Ok(());
    'tables: for table in registry.values() {
        let Some(sheet) = sheet_names
            .iter()
            .position(|name| name == &table.sheet_name)
            .map(|index| index as u32)
        else {
            result = Err(format!(
                "table {} references missing sheet {}",
                table.display_name, table.sheet_name
            ));
            break;
        };
        let Some((left, right)) = table.reference.split_once(':') else {
            result = Err(format!("invalid table range {}", table.reference));
            break;
        };
        let (Some((r0, c0)), Some((r1, c1))) = (parse_a1(left), parse_a1(right)) else {
            result = Err(format!("invalid table range {}", table.reference));
            break;
        };
        if table.header_row_count > 0 {
            for (offset, column) in table.columns.iter().enumerate() {
                if c0 + offset as i32 > c1 {
                    break;
                }
                if let Err(error) =
                    st.model
                        .set_user_input(sheet, r0, c0 + offset as i32, &column.name)
                {
                    result = Err(error);
                    break 'tables;
                }
            }
        }
        let data_start = r0 + table.header_row_count as i32;
        let data_end = r1 - table.totals_row_count as i32;
        let native_columns = model["tables"]
            .as_array()
            .and_then(|tables| {
                tables
                    .iter()
                    .find(|value| value["displayName"] == table.display_name)
            })
            .and_then(|value| value["columns"].as_array());
        if data_start <= data_end {
            if let Some(native_columns) = native_columns {
                for (offset, column) in native_columns.iter().enumerate() {
                    let Some(formula) = column
                        .get("calculatedColumnFormula")
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    let formula = explicit_calculated_table_formula(&table.display_name, formula);
                    for row in data_start..=data_end {
                        if let Some((scope_start, scope_end)) = calculated_rows {
                            if row < scope_start || row > scope_end {
                                continue;
                            }
                        }
                        let target_col = c0 + offset as i32;
                        if !force_calculated
                            && !st
                                .model
                                .get_cell_content(sheet, row, target_col)?
                                .is_empty()
                        {
                            continue;
                        }
                        if let Err(error) =
                            st.model.set_user_input(sheet, row, target_col, &formula)
                        {
                            result = Err(error);
                            break 'tables;
                        }
                    }
                }
            }
        }
        if table.totals_row_count > 0 {
            if let Some(native_columns) = native_columns {
                for (offset, column) in native_columns.iter().enumerate() {
                    let target_column = c0 + offset as i32;
                    let value =
                        if let Some(label) = column.get("totalsRowLabel").and_then(Value::as_str) {
                            Some(label.to_string())
                        } else if let Some(formula) =
                            column.get("totalsRowFormula").and_then(Value::as_str)
                        {
                            Some(explicit_totals_table_formula(
                                &table.display_name,
                                &table.columns,
                                formula,
                            ))
                        } else if let Some(function) =
                            column.get("totalsRowFunction").and_then(Value::as_str)
                        {
                            let code = match function {
                                "average" => 101,
                                "countNums" => 102,
                                "count" => 103,
                                "max" => 104,
                                "min" => 105,
                                "stdDev" => 107,
                                "sum" => 109,
                                "var" => 110,
                                _ => continue,
                            };
                            Some(format!(
                                "=SUBTOTAL({code},{}[{}])",
                                table.display_name, table.columns[offset].name
                            ))
                        } else {
                            None
                        };
                    if let Some(value) = value {
                        if let Err(error) =
                            st.model.set_user_input(sheet, r1, target_column, &value)
                        {
                            result = Err(error);
                            break 'tables;
                        }
                    }
                }
            }
        }
    }
    st.model.resume_evaluation();
    st.model.evaluate();
    result?;
    st.native_table_model = Some(model.clone());
    Ok(())
}

fn sync_native_table_cells(st: &mut AppState, model: &Value) -> Result<(), String> {
    sync_native_table_cells_scoped(st, model, None, false)
}

fn table_patch_sets_calculated_formula(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, child)| {
            (key == "calculatedColumnFormula" && !child.is_null())
                || table_patch_sets_calculated_formula(child)
        }),
        Value::Array(values) => values.iter().any(table_patch_sets_calculated_formula),
        _ => false,
    }
}

fn auto_expand_table_for_input(
    st: &mut AppState,
    sheet: u32,
    row: i32,
    col: i32,
) -> Result<bool, String> {
    let Some(cached) = st.native_table_model.clone() else {
        return Ok(false);
    };
    let sheet_name = st
        .model
        .get_model()
        .workbook
        .worksheet(sheet)?
        .get_name()
        .to_string();
    let Some((table, expands_row)) =
        cached
            .get("tables")
            .and_then(Value::as_array)
            .and_then(|tables| {
                tables.iter().find_map(|table| {
                    if table.get("sheet").and_then(Value::as_str) != Some(sheet_name.as_str()) {
                        return None;
                    }
                    let Some(reference) = table.get("reference").and_then(Value::as_str) else {
                        return None;
                    };
                    let Some((left, right)) = reference.split_once(':') else {
                        return None;
                    };
                    let (Some((r0, c0)), Some((r1, c1))) = (parse_a1(left), parse_a1(right)) else {
                        return None;
                    };
                    let totals = table
                        .get("totalsRowCount")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if totals == 0 && row == r1 + 1 && (c0..=c1).contains(&col) {
                        Some((table.clone(), true))
                    } else if col == c1 + 1 && (r0..=r1).contains(&row) {
                        Some((table.clone(), false))
                    } else {
                        None
                    }
                })
            })
    else {
        return Ok(false);
    };
    let reference = table["reference"]
        .as_str()
        .ok_or("table has no reference")?;
    let (left, right) = reference.split_once(':').ok_or("invalid table reference")?;
    let (r0, c0) = parse_a1(left).ok_or("invalid table start")?;
    let (old_r1, old_c1) = parse_a1(right).ok_or("invalid table end")?;
    let new_r1 = if expands_row { row } else { old_r1 };
    let new_c1 = if expands_row { old_c1 } else { col };
    let new_reference = format!("{}:{}", cell_ref_str(r0, c0), cell_ref_str(new_r1, new_c1));
    let mut table_patch = json!({"ref":new_reference});
    if let Some(auto_filter) = table.get("autoFilter").filter(|value| !value.is_null()) {
        let filter_reference = auto_filter
            .get("reference")
            .or_else(|| auto_filter.get("ref"))
            .and_then(Value::as_str)
            .and_then(|reference| reference.split_once(':'))
            .and_then(|(left, right)| Some((parse_a1(left)?, parse_a1(right)?)))
            .map(|((filter_r0, filter_c0), (filter_r1, _))| {
                let end_row = if expands_row { row } else { filter_r1 };
                format!(
                    "{}:{}",
                    cell_ref_str(filter_r0, filter_c0),
                    cell_ref_str(end_row, new_c1)
                )
            })
            .unwrap_or_else(|| new_reference.clone());
        table_patch["autoFilter"] = json!({"ref":filter_reference});
    }
    if !expands_row {
        let existing_names = table
            .get("columns")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let entered = st.model.get_cell_content(sheet, row, col)?;
        let header_rows = table
            .get("headerRowCount")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let base = if header_rows > 0 && row == r0 && !entered.trim().is_empty() {
            entered.trim().to_string()
        } else {
            format!("Column{}", existing_names.len() + 1)
        };
        let mut name = base.clone();
        let mut suffix = 2usize;
        while existing_names.iter().any(|column| {
            column
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|existing| existing.eq_ignore_ascii_case(&name))
        }) {
            name = format!("{base}{suffix}");
            suffix += 1;
        }
        let next_id = existing_names
            .iter()
            .filter_map(|column| column.get("id").and_then(Value::as_u64))
            .max()
            .unwrap_or(0)
            + 1;
        table_patch["columnOperations"] = json!([{"op":"add","patch":{"id":next_id,"name":name}}]);
    }
    let patch = json!({
        "tableEdits":[{
            "part":table["part"],
            "patch":table_patch
        }]
    });
    let mut parts = materialize_table_feature_parts(st)?;
    native_table_edit::apply_table_package_edit(&mut parts, &patch)?;
    st.native_table_edits.push(patch);
    // Any pre-existing non-empty value in the appended row is a manual calculated-column
    // exception.  Multi-cell paste writes the entire row before expansion, so preserve all of
    // those cells while filling formulas into the remaining blanks.
    let manual_cells = if expands_row {
        (c0..=old_c1)
            .map(|column| Ok((column, st.model.get_cell_content(sheet, row, column)?)))
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .filter(|(_, content)| !content.is_empty())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let updated = table_model_with_edits(st)?;
    sync_native_table_cells_scoped(st, &updated, expands_row.then_some((row, row)), false)?;
    let mut restored = false;
    for (column, content) in manual_cells {
        if st.model.get_cell_content(sheet, row, column)? != content {
            st.model.set_user_input(sheet, row, column, &content)?;
            restored = true;
        }
    }
    if restored {
        st.model.evaluate();
    }
    Ok(true)
}

fn auto_expand_tables_for_cells(
    st: &mut AppState,
    sheet: u32,
    cells: &[(i32, i32)],
) -> Result<(), String> {
    if st.native_table_model.is_none() || cells.is_empty() {
        return Ok(());
    }
    let mut cells = cells.to_vec();
    cells.sort_unstable();
    cells.dedup();
    for (row, col) in cells {
        auto_expand_table_for_input(st, sheet, row, col)?;
    }
    Ok(())
}

fn auto_expand_tables_for_area(
    st: &mut AppState,
    sheet: u32,
    r0: i32,
    c0: i32,
    r1: i32,
    c1: i32,
) -> Result<(), String> {
    if st.native_table_model.is_none() {
        return Ok(());
    }
    let sheet_name = st
        .model
        .get_model()
        .workbook
        .worksheet(sheet)?
        .get_name()
        .to_string();
    for row in r0..=r1 {
        loop {
            let candidate_col = st
                .native_table_model
                .as_ref()
                .and_then(|model| model.get("tables"))
                .and_then(Value::as_array)
                .and_then(|tables| {
                    tables.iter().find_map(|table| {
                        if table.get("sheet").and_then(Value::as_str) != Some(sheet_name.as_str())
                            || table
                                .get("totalsRowCount")
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                                != 0
                        {
                            return None;
                        }
                        let reference = table.get("reference").and_then(Value::as_str)?;
                        let (left, right) = reference.split_once(':')?;
                        let (_, table_c0) = parse_a1(left)?;
                        let (table_r1, table_c1) = parse_a1(right)?;
                        if row == table_r1 + 1 && c0 <= table_c1 && c1 >= table_c0 {
                            Some(c0.max(table_c0))
                        } else {
                            None
                        }
                    })
                });
            let Some(candidate_col) = candidate_col else {
                break;
            };
            if !auto_expand_table_for_input(st, sheet, row, candidate_col)? {
                break;
            }
        }
    }
    for col in c0..=c1 {
        loop {
            let candidate_row = st
                .native_table_model
                .as_ref()
                .and_then(|model| model.get("tables"))
                .and_then(Value::as_array)
                .and_then(|tables| {
                    tables.iter().find_map(|table| {
                        if table.get("sheet").and_then(Value::as_str) != Some(sheet_name.as_str()) {
                            return None;
                        }
                        let reference = table.get("reference").and_then(Value::as_str)?;
                        let (left, right) = reference.split_once(':')?;
                        let (table_r0, _) = parse_a1(left)?;
                        let (table_r1, table_c1) = parse_a1(right)?;
                        if col == table_c1 + 1 && r0 <= table_r1 && r1 >= table_r0 {
                            Some(r0.max(table_r0))
                        } else {
                            None
                        }
                    })
                });
            let Some(candidate_row) = candidate_row else {
                break;
            };
            if !auto_expand_table_for_input(st, sheet, candidate_row, col)? {
                break;
            }
        }
    }
    Ok(())
}

fn api_tables(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = if body.is_empty() {
        json!({"op":"list"})
    } else {
        parse_body(body)?
    };
    match request.get("op").and_then(Value::as_str).unwrap_or("list") {
        "list" => ok_json(table_model_with_edits(st)?),
        "update" | "apply" | "create" | "delete" => {
            let patch = request
                .get("patch")
                .ok_or("missing native table package patch")?;
            let mut parts = materialize_table_feature_parts(st)?;
            let before = parts.clone();
            native_table_edit::apply_table_package_edit(&mut parts, patch)?;
            if parts != before {
                st.native_table_edits.push(patch.clone());
            }
            let updated_model = table_model_with_edits(st)?;
            sync_native_table_cells_scoped(
                st,
                &updated_model,
                None,
                table_patch_sets_calculated_formula(patch),
            )?;
            // The native XML definition and the visible worksheet operation are one Excel
            // command.  Execute optional runtime sort/filter payloads inside this same API
            // transaction so Ctrl+Z restores both row order/visibility and OOXML metadata.
            if let Some(operations) = request.get("runtime").and_then(Value::as_array) {
                for operation in operations {
                    let kind = operation
                        .get("type")
                        .or_else(|| operation.get("op"))
                        .and_then(Value::as_str)
                        .ok_or("table runtime operation requires type")?;
                    let payload = operation.get("request").unwrap_or(operation);
                    let bytes = serde_json::to_vec(payload)
                        .map_err(|error| format!("table runtime payload: {error}"))?;
                    match kind {
                        "sort" => {
                            api_sort_range(st, &bytes)?;
                        }
                        "filter" => {
                            api_filter_range(st, &bytes)?;
                        }
                        other => {
                            return Err(format!("unsupported table runtime operation: {other}"));
                        }
                    }
                }
            }
            ok_json(updated_model)
        }
        "reset" => {
            st.native_table_edits.clear();
            let model = table_model_with_edits(st)?;
            sync_native_table_cells(st, &model)?;
            ok_json(model)
        }
        other => Err(format!("bad native table op: {other}")),
    }
}

fn materialize_page_review_parts(
    st: &AppState,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let mut parts = if let Some(snapshot) = st.source_ooxml.as_ref() {
        snapshot.parts.clone()
    } else {
        snapshot_opc_package(&model_to_xlsx_bytes(st)?)?.parts
    };
    apply_native_data_edit_journal(&mut parts, &st.native_data_edits)?;
    apply_table_edit_journal(&mut parts, &st.native_table_edits)?;
    apply_pivot_local_refresh_journal(&mut parts, &st.native_pivot_local_refresh_edits)?;
    apply_pivot_table_edit_journal(&mut parts, &st.native_pivot_table_edits)?;
    apply_slicer_edit_journal(&mut parts, &st.native_slicer_edits)?;
    apply_timeline_edit_journal(&mut parts, &st.native_timeline_edits)?;
    apply_page_review_edit_journal(&mut parts, &st.native_page_review_edits)?;
    Ok(parts)
}

fn page_review_model_with_edits(st: &AppState) -> Result<Value, String> {
    native_page_review_edit::inspect_page_review_model(&materialize_page_review_parts(st)?)
}

fn clear_password_fields(object: &mut serde_json::Map<String, Value>, prefix: Option<&str>) {
    let field = |name: &str| match prefix {
        Some(prefix) => format!("{prefix}{}{}", name[..1].to_ascii_uppercase(), &name[1..]),
        None => name.to_string(),
    };
    for name in [
        "password",
        "algorithmName",
        "hashValue",
        "saltValue",
        "spinCount",
    ] {
        object.insert(field(name), Value::Null);
    }
}

fn apply_password_change(
    object: &mut serde_json::Map<String, Value>,
    change: &Value,
    prefix: Option<&str>,
) -> Result<(), String> {
    clear_password_fields(object, prefix);
    if change.get("clear").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let password = change
        .get("password")
        .and_then(Value::as_str)
        .ok_or("password change requires password or clear=true")?;
    object.extend(protection_runtime::password_hash_attributes(
        password, prefix,
    )?);
    Ok(())
}

fn worksheet_patch_mut<'a>(
    patch: &'a mut Value,
    sheet_name: &str,
) -> Result<&'a mut Value, String> {
    let patch_object = patch
        .as_object_mut()
        .ok_or("page-review patch must be an object")?;
    let worksheets = patch_object
        .entry("worksheets")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or("page-review patch.worksheets must be an array")?;
    let index = worksheets
        .iter()
        .position(|worksheet| worksheet.get("sheet").and_then(Value::as_str) == Some(sheet_name));
    let index = index.unwrap_or_else(|| {
        worksheets.push(json!({"sheet":sheet_name}));
        worksheets.len() - 1
    });
    Ok(&mut worksheets[index])
}

/// Converts transient plaintext password commands into Excel's salted SHA-512 attributes before
/// the patch reaches either the OOXML journal or application history. Plaintext is never retained.
fn prepare_page_review_patch(request: &Value) -> Result<Value, String> {
    let mut patch = request
        .get("patch")
        .cloned()
        .unwrap_or_else(|| request.clone());
    let Some(changes) = request.get("passwordChanges") else {
        return Ok(patch);
    };
    if let Some(change) = changes.get("workbook") {
        let workbook = patch
            .as_object_mut()
            .ok_or("page-review patch must be an object")?
            .entry("workbookProtection")
            .or_insert_with(|| Value::Object(Default::default()))
            .as_object_mut()
            .ok_or("cannot set a password while workbook protection is disabled")?;
        apply_password_change(workbook, change, Some("workbook"))?;
    }
    if let Some(worksheets) = changes.get("worksheets").and_then(Value::as_array) {
        for change in worksheets {
            let sheet_name = change
                .get("sheet")
                .and_then(Value::as_str)
                .ok_or("worksheet password change requires sheet")?;
            let worksheet = worksheet_patch_mut(&mut patch, sheet_name)?;
            let protection = worksheet
                .as_object_mut()
                .ok_or("worksheet patch must be an object")?
                .entry("sheetProtection")
                .or_insert_with(|| Value::Object(Default::default()))
                .as_object_mut()
                .ok_or("cannot set a password while sheet protection is disabled")?;
            apply_password_change(protection, change, None)?;
        }
    }
    if let Some(ranges) = changes.get("protectedRanges").and_then(Value::as_array) {
        for change in ranges {
            let sheet_name = change
                .get("sheet")
                .and_then(Value::as_str)
                .ok_or("protected-range password change requires sheet")?;
            let range_name = change
                .get("name")
                .and_then(Value::as_str)
                .ok_or("protected-range password change requires name")?;
            let worksheet = worksheet_patch_mut(&mut patch, sheet_name)?;
            let worksheet = worksheet
                .as_object_mut()
                .ok_or("worksheet patch must be an object")?;
            let ranges_patch = worksheet
                .entry("protectedRanges")
                .or_insert_with(|| json!({"upsert":[]}))
                .as_object_mut()
                .ok_or("protectedRanges password patch conflicts with a replacement patch")?;
            let upsert = ranges_patch
                .entry("upsert")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or("protectedRanges.upsert must be an array")?;
            let index = upsert
                .iter()
                .position(|range| range.get("name").and_then(Value::as_str) == Some(range_name));
            let index = index.unwrap_or_else(|| {
                upsert.push(json!({"name":range_name}));
                upsert.len() - 1
            });
            let range = upsert[index]
                .as_object_mut()
                .ok_or("protected range patch must be an object")?;
            apply_password_change(range, change, None)?;
        }
    }
    Ok(patch)
}

fn api_page_review(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = if body.is_empty() {
        json!({"op":"list"})
    } else {
        parse_body(body)?
    };
    let op = request
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or(if body.is_empty() { "list" } else { "update" });
    match op {
        "list" | "get" | "inspect" => ok_json(page_review_model_with_edits(st)?),
        "update" | "apply" => {
            let current = page_review_model_with_edits(st)?;
            let patch = prepare_page_review_patch(&request)?;
            let mut authorization_request = request.clone();
            authorization_request
                .as_object_mut()
                .ok_or("page-review request must be an object")?
                .insert("patch".into(), patch.clone());
            protection_runtime::authorize_protection_edit(&current, &authorization_request)?;
            let mut parts = materialize_page_review_parts(st)?;
            let before = parts.clone();
            native_page_review_edit::apply_page_review_package_edit(&mut parts, &patch)?;
            if parts != before {
                st.native_page_review_edits.push(patch);
            }
            ok_json(page_review_model_with_edits(st)?)
        }
        "reset" => {
            let current = page_review_model_with_edits(st)?;
            protection_runtime::authorize_protection_edit(&current, &request)?;
            st.native_page_review_edits.clear();
            ok_json(page_review_model_with_edits(st)?)
        }
        other => Err(format!("bad page/review op: {other}")),
    }
}

fn sort_value_compare(
    left: &CellValue,
    right: &CellValue,
    case_sensitive: bool,
) -> std::cmp::Ordering {
    fn rank(value: &CellValue) -> u8 {
        match value {
            CellValue::Number(_) => 0,
            CellValue::String(value) if !value.is_empty() => 1,
            CellValue::Boolean(_) => 2,
            CellValue::None | CellValue::String(_) => 3,
        }
    }
    let ordering = rank(left).cmp(&rank(right));
    if ordering != std::cmp::Ordering::Equal {
        return ordering;
    }
    match (left, right) {
        (CellValue::Number(left), CellValue::Number(right)) => {
            left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
        }
        (CellValue::Boolean(left), CellValue::Boolean(right)) => left.cmp(right),
        (CellValue::String(left), CellValue::String(right)) => {
            if case_sensitive {
                left.cmp(right)
            } else {
                left.to_lowercase().cmp(&right.to_lowercase())
            }
        }
        _ => std::cmp::Ordering::Equal,
    }
}

fn api_sort_range(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = parse_body(body)?;
    let sheet = ji(&request, "sheet")? as u32;
    let (r0, c0, r1, c1) = clamp_range(
        ji(&request, "r0")? as i32,
        ji(&request, "c0")? as i32,
        ji(&request, "r1")? as i32,
        ji(&request, "c1")? as i32,
    );
    let header_rows = request
        .get("headerRows")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            request
                .get("hasHeader")
                .and_then(Value::as_bool)
                .map(i64::from)
                .unwrap_or(1)
        })
        .clamp(0, (r1 - r0) as i64) as i32;
    let first_data_row = r0 + header_rows;
    let conditions = request
        .get("conditions")
        .and_then(Value::as_array)
        .ok_or("sort requires conditions")?;
    if conditions.is_empty() {
        return Err("sort requires at least one condition".into());
    }
    for condition in conditions {
        let column = ji(condition, "col")? as i32;
        if !(c0..=c1).contains(&column) {
            return Err(format!("sort column {column} is outside the range"));
        }
    }
    if merged_ranges(st, sheet)?
        .iter()
        .any(|&(mr0, mc0, mr1, mc1)| mr1 >= first_data_row && mr0 <= r1 && mc1 >= c0 && mc0 <= c1)
    {
        return Err("cannot sort a range containing merged cells".into());
    }

    #[derive(Clone)]
    struct CapturedSortCell {
        content: String,
        style: Style,
        rich: Option<Vec<RichTextRun>>,
        rich_xml: Option<String>,
    }
    struct CapturedSortRow {
        source_row: i32,
        cells: Vec<CapturedSortCell>,
        keys: Vec<CellValue>,
    }

    let mut rows = Vec::new();
    for row in first_data_row..=r1 {
        let mut cells = Vec::new();
        for column in c0..=c1 {
            cells.push(CapturedSortCell {
                content: st.model.get_cell_content(sheet, row, column)?,
                style: st.model.get_cell_style(sheet, row, column)?,
                rich: st.rich_text.get(&(sheet, row, column)).cloned(),
                rich_xml: st.rich_text_xml.get(&(sheet, row, column)).cloned(),
            });
        }
        let keys = conditions
            .iter()
            .map(|condition| {
                let column = ji(condition, "col")? as i32;
                let sort_on = condition
                    .get("sortOn")
                    .and_then(Value::as_str)
                    .unwrap_or("values");
                if sort_on == "cellColor" || sort_on == "fontColor" {
                    let style = st.model.get_cell_style(sheet, row, column)?;
                    let actual = if sort_on == "cellColor" {
                        st.model.resolve_color(&style.fill.color)
                    } else {
                        st.model.resolve_color(&style.font.color)
                    };
                    let source = if sort_on == "fontColor" {
                        "font"
                    } else {
                        "fill"
                    };
                    let wanted = condition
                        .get("color")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| {
                            condition
                                .get("dxfId")
                                .and_then(Value::as_u64)
                                .and_then(|id| native_dxf_filter_color(st, id as usize, source))
                        })
                        .unwrap_or_default();
                    Ok(CellValue::Number(
                        if !wanted.is_empty() && actual.eq_ignore_ascii_case(&wanted) {
                            0.0
                        } else {
                            1.0
                        },
                    ))
                } else {
                    st.model
                        .get_model()
                        .get_cell_value_by_index(sheet, row, column)
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        rows.push(CapturedSortRow {
            source_row: row,
            cells,
            keys,
        });
    }
    let case_sensitive = request
        .get("caseSensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    rows.sort_by(|left, right| {
        for (index, condition) in conditions.iter().enumerate() {
            let mut ordering =
                sort_value_compare(&left.keys[index], &right.keys[index], case_sensitive);
            if condition
                .get("descending")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || condition.get("order").and_then(Value::as_str) == Some("descending")
            {
                ordering = ordering.reverse();
            }
            if ordering != std::cmp::Ordering::Equal {
                return ordering;
            }
        }
        std::cmp::Ordering::Equal
    });

    let row_map = rows
        .iter()
        .enumerate()
        .map(|(offset, row_data)| (row_data.source_row, first_data_row + offset as i32))
        .collect::<std::collections::HashMap<_, _>>();
    let mut styles = Vec::with_capacity(rows.len());
    st.model.pause_evaluation();
    let mut write_result = Ok(());
    for (offset, row_data) in rows.iter().enumerate() {
        let target_row = first_data_row + offset as i32;
        let mut style_row = Vec::with_capacity(row_data.cells.len());
        for (column_offset, cell) in row_data.cells.iter().enumerate() {
            let column = c0 + column_offset as i32;
            let source = CellReferenceIndex {
                sheet,
                row: row_data.source_row,
                column,
            };
            let target = CellReferenceIndex {
                sheet,
                row: target_row,
                column,
            };
            let content = match st
                .model
                .extend_copied_value(&cell.content, &source, &target)
            {
                Ok(content) => content,
                Err(error) => {
                    write_result = Err(error);
                    break;
                }
            };
            if let Err(error) = st.model.set_user_input(sheet, target_row, column, &content) {
                write_result = Err(error);
                break;
            }
            style_row.push(cell.style.clone());
        }
        styles.push(style_row);
        if write_result.is_err() {
            break;
        }
    }
    if write_result.is_ok() {
        st.rich_text.retain(|(s, r, c), _| {
            !(*s == sheet && *r >= first_data_row && *r <= r1 && *c >= c0 && *c <= c1)
        });
        st.rich_text_xml.retain(|(s, r, c), _| {
            !(*s == sheet && *r >= first_data_row && *r <= r1 && *c >= c0 && *c <= c1)
        });
        for (offset, row_data) in rows.iter().enumerate() {
            let target_row = first_data_row + offset as i32;
            for (column_offset, cell) in row_data.cells.iter().enumerate() {
                let column = c0 + column_offset as i32;
                if let Some(runs) = &cell.rich {
                    let content = runs.iter().map(|run| run.text.as_str()).collect::<String>();
                    if let Err(error) = st
                        .model
                        .set_rich_text_plain_value(sheet, target_row, column, &content)
                    {
                        write_result = Err(error);
                        break;
                    }
                    st.rich_text
                        .insert((sheet, target_row, column), runs.clone());
                    if let Some(xml) = &cell.rich_xml {
                        st.rich_text_xml
                            .insert((sheet, target_row, column), xml.clone());
                    }
                }
            }
            if write_result.is_err() {
                break;
            }
        }
    }
    st.model.resume_evaluation();
    st.model.evaluate();
    write_result?;
    st.model.set_selected_sheet(sheet)?;
    st.model.set_selected_cell(first_data_row, c0)?;
    st.model.set_selected_range(first_data_row, c0, r1, c1)?;
    st.model.on_paste_styles(&styles)?;

    if let Some(objects) = st.objects.get_mut(&sheet) {
        for object in objects {
            let Some(source_row) = object
                .get("r")
                .and_then(Value::as_i64)
                .map(|row| row as i32)
            else {
                continue;
            };
            if let Some(target_row) = row_map.get(&source_row) {
                object["r"] = json!(target_row);
            }
        }
    }
    ok_json(json!({
        "sorted": rows.len(),
        "range":{"r0":first_data_row,"c0":c0,"r1":r1,"c1":c1}
    }))
}

fn wildcard_matches(value: &str, pattern: &str, case_sensitive: bool) -> bool {
    let value = if case_sensitive {
        value.to_string()
    } else {
        value.to_lowercase()
    };
    let pattern = if case_sensitive {
        pattern.to_string()
    } else {
        pattern.to_lowercase()
    };
    let value = value.chars().collect::<Vec<_>>();
    let pattern = pattern.chars().collect::<Vec<_>>();
    let mut table = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    table[0][0] = true;
    for p in 1..=pattern.len() {
        if pattern[p - 1] == '*' {
            table[p][0] = table[p - 1][0];
        }
        for v in 1..=value.len() {
            table[p][v] = match pattern[p - 1] {
                '*' => table[p - 1][v] || table[p][v - 1],
                '?' => table[p - 1][v - 1],
                ch => ch == value[v - 1] && table[p - 1][v - 1],
            };
        }
    }
    table[pattern.len()][value.len()]
}

fn custom_filter_condition(
    actual: &str,
    operator: &str,
    expected: &str,
    case_sensitive: bool,
) -> bool {
    let numeric = actual.parse::<f64>().ok().zip(expected.parse::<f64>().ok());
    let ordering = numeric
        .and_then(|(left, right)| left.partial_cmp(&right))
        .unwrap_or_else(|| {
            if case_sensitive {
                actual.cmp(expected)
            } else {
                actual.to_lowercase().cmp(&expected.to_lowercase())
            }
        });
    match operator {
        "notEqual" | "not_equal" => {
            if expected.contains('*') || expected.contains('?') {
                !wildcard_matches(actual, expected, case_sensitive)
            } else {
                ordering != std::cmp::Ordering::Equal
            }
        }
        "greaterThan" | "greater_than" => ordering == std::cmp::Ordering::Greater,
        "greaterThanOrEqual" | "greater_than_or_equal" => ordering != std::cmp::Ordering::Less,
        "lessThan" | "less_than" => ordering == std::cmp::Ordering::Less,
        "lessThanOrEqual" | "less_than_or_equal" => ordering != std::cmp::Ordering::Greater,
        "beginsWith" => {
            if case_sensitive {
                actual.starts_with(expected)
            } else {
                actual.to_lowercase().starts_with(&expected.to_lowercase())
            }
        }
        "endsWith" => {
            if case_sensitive {
                actual.ends_with(expected)
            } else {
                actual.to_lowercase().ends_with(&expected.to_lowercase())
            }
        }
        "contains" => {
            if case_sensitive {
                actual.contains(expected)
            } else {
                actual.to_lowercase().contains(&expected.to_lowercase())
            }
        }
        "notContains" => {
            if case_sensitive {
                !actual.contains(expected)
            } else {
                !actual.to_lowercase().contains(&expected.to_lowercase())
            }
        }
        _ => {
            if expected.contains('*') || expected.contains('?') {
                wildcard_matches(actual, expected, case_sensitive)
            } else {
                ordering == std::cmp::Ordering::Equal
            }
        }
    }
}

fn native_dxf_filter_color(st: &AppState, dxf_id: usize, source: &str) -> Option<String> {
    let styles = st
        .source_ooxml
        .as_ref()?
        .parts
        .get("xl/styles.xml")
        .and_then(|bytes| std::str::from_utf8(bytes).ok())?;
    let document = roxmltree::Document::parse(styles).ok()?;
    let dxfs = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "dxfs")?;
    let dxf = dxfs
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "dxf")
        .nth(dxf_id)?;
    let container_name = if source == "font" { "font" } else { "fill" };
    let container = dxf
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == container_name)?;
    let color = container.descendants().find(|node| {
        node.is_element() && matches!(node.tag_name().name(), "color" | "fgColor" | "bgColor")
    })?;
    if let Some(rgb) = color.attribute("rgb") {
        let rgb = if rgb.len() == 8 { &rgb[2..] } else { rgb };
        if rgb.len() == 6 && rgb.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Some(format!("#{}", rgb.to_ascii_uppercase()));
        }
    }
    if let Some(theme) = color
        .attribute("theme")
        .and_then(|value| value.parse::<i32>().ok())
    {
        let tint = color
            .attribute("tint")
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0);
        return Some(st.model.resolve_color(&Color::Theme(theme, tint)));
    }
    None
}

fn excel_serial_components(serial: f64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    if !serial.is_finite() {
        return None;
    }
    // Excel's 1900 date system maps 1970-01-01 to serial 25569.  Convert the
    // integral day with the proleptic Gregorian civil-from-days algorithm.
    let day = serial.floor() as i64 - 25_569;
    let z = day + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day_of_month = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let seconds = ((serial - serial.floor()) * 86_400.0).round() as i64;
    let seconds = seconds.rem_euclid(86_400);
    Some((
        year as i32,
        month as u32,
        day_of_month as u32,
        (seconds / 3_600) as u32,
        ((seconds % 3_600) / 60) as u32,
        (seconds % 60) as u32,
    ))
}

fn date_group_matches(serial: f64, group: &Value) -> bool {
    let Some((year, month, day, hour, minute, second)) = excel_serial_components(serial) else {
        return false;
    };
    let matches = |name: &str, actual: i64| {
        group
            .get(name)
            .or_else(|| group.get("attributes").and_then(|attrs| attrs.get(name)))
            .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
            .is_none_or(|expected| expected == actual)
    };
    matches("year", year as i64)
        && matches("month", month as i64)
        && matches("day", day as i64)
        && matches("hour", hour as i64)
        && matches("minute", minute as i64)
        && matches("second", second as i64)
}

fn filter_matches(st: &AppState, sheet: u32, row: i32, filter: &Value) -> Result<bool, String> {
    let column = ji(filter, "col")? as i32;
    let formatted = st.model.get_formatted_cell_value(sheet, row, column)?;
    let content = st.model.get_cell_content(sheet, row, column)?;
    let case_sensitive = filter
        .get("caseSensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match filter
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("values")
    {
        "values" => {
            let include_blank = filter
                .get("includeBlank")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if formatted.is_empty() {
                return Ok(include_blank);
            }
            let values = filter
                .get("values")
                .and_then(Value::as_array)
                .ok_or("values filter requires values")?;
            Ok(values.iter().filter_map(Value::as_str).any(|value| {
                if case_sensitive {
                    formatted == value
                } else {
                    formatted.eq_ignore_ascii_case(value)
                }
            }))
        }
        "custom" => {
            // Excel compares the underlying numeric/date serial when both the criterion and the
            // cell are numeric; formatted text is only the fallback for textual filters.
            let expected = filter.get("value").and_then(Value::as_str).unwrap_or("");
            let actual = if expected.parse::<f64>().is_ok() && content.parse::<f64>().is_ok() {
                content.as_str()
            } else {
                formatted.as_str()
            };
            let first = custom_filter_condition(
                actual,
                filter
                    .get("operator")
                    .and_then(Value::as_str)
                    .unwrap_or("equal"),
                expected,
                case_sensitive,
            );
            let Some(second) = filter.get("second").and_then(Value::as_object) else {
                return Ok(first);
            };
            let second_expected = second.get("value").and_then(Value::as_str).unwrap_or("");
            let second_actual =
                if second_expected.parse::<f64>().is_ok() && content.parse::<f64>().is_ok() {
                    content.as_str()
                } else {
                    formatted.as_str()
                };
            let second_match = custom_filter_condition(
                second_actual,
                second
                    .get("operator")
                    .and_then(Value::as_str)
                    .unwrap_or("equal"),
                second_expected,
                case_sensitive,
            );
            Ok(
                if filter.get("join").and_then(Value::as_str) == Some("or") {
                    first || second_match
                } else {
                    first && second_match
                },
            )
        }
        "dateGroups" => {
            let Some(serial) = content.parse::<f64>().ok() else {
                return Ok(false);
            };
            let groups = filter
                .get("groups")
                .and_then(Value::as_array)
                .ok_or("dateGroups filter requires groups")?;
            Ok(groups.iter().any(|group| date_group_matches(serial, group)))
        }
        "top10" | "dynamic" => {
            let value = content
                .parse::<f64>()
                .or_else(|_| formatted.parse::<f64>())
                .ok();
            let threshold = filter.get("_threshold").and_then(Value::as_f64);
            Ok(
                match (
                    value,
                    threshold,
                    filter.get("direction").and_then(Value::as_str),
                ) {
                    (Some(value), Some(threshold), Some("bottom" | "belowAverage")) => {
                        value <= threshold
                    }
                    (Some(value), Some(threshold), _) => value >= threshold,
                    _ => false,
                },
            )
        }
        "color" => {
            let style = st.model.get_cell_style(sheet, row, column)?;
            let actual = if filter.get("source").and_then(Value::as_str) == Some("font") {
                st.model.resolve_color(&style.font.color)
            } else {
                st.model.resolve_color(&style.fill.color)
            };
            let source = filter
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("fill");
            let wanted = filter
                .get("color")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    filter
                        .get("dxfId")
                        .and_then(Value::as_u64)
                        .and_then(|id| native_dxf_filter_color(st, id as usize, source))
                })
                .unwrap_or_default();
            Ok(!wanted.is_empty() && actual.eq_ignore_ascii_case(&wanted))
        }
        other => Err(format!("unsupported runtime filter kind: {other}")),
    }
}

fn api_filter_range(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = parse_body(body)?;
    let sheet = ji(&request, "sheet")? as u32;
    let (r0, c0, r1, c1) = clamp_range(
        ji(&request, "r0")? as i32,
        ji(&request, "c0")? as i32,
        ji(&request, "r1")? as i32,
        ji(&request, "c1")? as i32,
    );
    let header_rows = request
        .get("headerRows")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(0, (r1 - r0) as i64) as i32;
    let first_data_row = r0 + header_rows;
    if request
        .get("clear")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        st.model.set_rows_hidden(sheet, first_data_row, r1, false)?;
        return ok_json(json!({"visible":r1-first_data_row+1,"hidden":0,"cleared":true}));
    }
    let mut filters = request
        .get("filters")
        .and_then(Value::as_array)
        .cloned()
        .ok_or("filter requires filters")?;
    for filter in &mut filters {
        let column = ji(filter, "col")? as i32;
        if !(c0..=c1).contains(&column) {
            return Err(format!("filter column {column} is outside the range"));
        }
        let kind = filter
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("values");
        if kind == "top10" || kind == "dynamic" {
            let mut values = (first_data_row..=r1)
                .filter_map(|row| {
                    st.model
                        .get_cell_content(sheet, row, column)
                        .ok()?
                        .parse::<f64>()
                        .ok()
                })
                .collect::<Vec<_>>();
            if values.is_empty() {
                filter["_threshold"] = Value::Null;
                continue;
            }
            values.sort_by(|left, right| {
                left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
            });
            let threshold = if kind == "dynamic" {
                values.iter().sum::<f64>() / values.len() as f64
            } else {
                let mut count = filter.get("count").and_then(Value::as_u64).unwrap_or(10) as usize;
                if filter
                    .get("percent")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    count = ((values.len() as f64 * count as f64 / 100.0).ceil() as usize).max(1);
                }
                count = count.clamp(1, values.len());
                if filter.get("direction").and_then(Value::as_str) == Some("bottom") {
                    values[count - 1]
                } else {
                    values[values.len() - count]
                }
            };
            filter["_threshold"] = json!(threshold);
        }
    }
    let mut visibility = Vec::new();
    let mut visible = 0i32;
    for row in first_data_row..=r1 {
        let show = filters
            .iter()
            .map(|filter| filter_matches(st, sheet, row, filter))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .all(|matches| matches);
        visible += i32::from(show);
        visibility.push((row, !show));
    }
    let mut index = 0usize;
    while index < visibility.len() {
        let hidden = visibility[index].1;
        let start = visibility[index].0;
        let mut end = start;
        index += 1;
        while index < visibility.len() && visibility[index].1 == hidden {
            end = visibility[index].0;
            index += 1;
        }
        st.model.set_rows_hidden(sheet, start, end, hidden)?;
    }
    ok_json(json!({
        "visible":visible,
        "hidden":(r1-first_data_row+1)-visible,
        "range":{"r0":first_data_row,"c0":c0,"r1":r1,"c1":c1}
    }))
}

fn materialize_native_feature_parts(
    st: &AppState,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let mut parts = st
        .source_ooxml
        .as_ref()
        .ok_or("workbook has no imported native OOXML package")?
        .parts
        .clone();
    apply_native_data_edit_journal(&mut parts, &st.native_data_edits)?;
    apply_table_edit_journal(&mut parts, &st.native_table_edits)?;
    apply_pivot_local_refresh_journal(&mut parts, &st.native_pivot_local_refresh_edits)?;
    apply_pivot_table_edit_journal(&mut parts, &st.native_pivot_table_edits)?;
    apply_slicer_edit_journal(&mut parts, &st.native_slicer_edits)?;
    // Timeline edits are intentionally replayed last because their date-range state owns the
    // corresponding native PivotTable date filter.
    apply_timeline_edit_journal(&mut parts, &st.native_timeline_edits)?;
    Ok(parts)
}

fn pivot_table_model_with_edits(st: &AppState) -> Result<Value, String> {
    let parts = materialize_native_feature_parts(st)?;
    let mut model = native_pivot_table_edit::parse_pivot_table_model(&parts)?;
    let edited_parts: std::collections::HashSet<&str> = st
        .native_pivot_table_edits
        .iter()
        .filter_map(|edit| edit.get("part").and_then(Value::as_str))
        .collect();
    if let Some(tables) = model.get_mut("tables").and_then(Value::as_array_mut) {
        for table in tables {
            let edited = table
                .get("part")
                .and_then(Value::as_str)
                .map(|part| edited_parts.contains(part))
                .unwrap_or(false);
            if let Some(object) = table.as_object_mut() {
                object.insert("edited".to_string(), Value::Bool(edited));
            }
        }
    }
    Ok(model)
}

fn mark_native_pivot_cache_for_refresh(st: &mut AppState, cache_part: Option<&str>) {
    let Some(part) = cache_part else {
        return;
    };
    let mut patch = st
        .pivot_cache_refresh_edits
        .get(part)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    patch.insert("refreshOnLoad".to_string(), Value::Bool(true));
    patch.insert("enableRefresh".to_string(), Value::Bool(true));
    st.pivot_cache_refresh_edits
        .insert(part.to_string(), Value::Object(patch));
}

fn api_pivot_tables(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = if body.is_empty() {
        json!({"op":"list"})
    } else {
        parse_body(body)?
    };
    match request.get("op").and_then(Value::as_str).unwrap_or("list") {
        "list" => ok_json(pivot_table_model_with_edits(st)?),
        "update" => {
            let part = request
                .get("part")
                .and_then(Value::as_str)
                .ok_or("missing PivotTable part")?;
            let patch = request.get("patch").ok_or("missing PivotTable patch")?;
            let mut parts = materialize_native_feature_parts(st)?;
            let current_model = native_pivot_table_edit::parse_pivot_table_model(&parts)?;
            let table = current_model["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|table| table["part"].as_str() == Some(part))
                .ok_or("PivotTable stable part key is unavailable")?;
            let cache_part = table["cachePart"].as_str().map(str::to_string);
            let original = parts
                .get(part)
                .ok_or("PivotTable part is unavailable")?
                .clone();
            let original_xml = std::str::from_utf8(&original)
                .map_err(|error| format!("PivotTable UTF-8: {error}"))?;
            let edited = native_pivot_table_edit::apply_pivot_table_patch(original_xml, patch)?;
            if edited.as_bytes() != original.as_slice() {
                parts.insert(part.to_string(), edited.into_bytes());
                // Re-parse the whole relationship-resolved model before committing the journal.
                native_pivot_table_edit::parse_pivot_table_model(&parts)?;
                st.native_pivot_table_edits
                    .push(json!({"part":part,"patch":patch.clone()}));
                mark_native_pivot_cache_for_refresh(st, cache_part.as_deref());
            }
            ok_json(pivot_table_model_with_edits(st)?)
        }
        "reset" => {
            if let Some(part) = request.get("part").and_then(Value::as_str) {
                st.native_pivot_table_edits
                    .retain(|edit| edit.get("part").and_then(Value::as_str) != Some(part));
            } else {
                st.native_pivot_table_edits.clear();
            }
            ok_json(pivot_table_model_with_edits(st)?)
        }
        other => Err(format!("bad PivotTable op: {other}")),
    }
}

fn pivot_request_with_model_source(st: &AppState, request: &Value) -> Result<Value, String> {
    let Some(range) = request.get("sourceRange").and_then(Value::as_object) else {
        return Ok(request.clone());
    };
    let sheet = range.get("sheet").and_then(Value::as_u64).unwrap_or(0) as u32;
    let (r0, c0, r1, c1) = clamp_range(
        range.get("r0").and_then(Value::as_i64).unwrap_or(1) as i32,
        range.get("c0").and_then(Value::as_i64).unwrap_or(1) as i32,
        range.get("r1").and_then(Value::as_i64).unwrap_or(1) as i32,
        range.get("c1").and_then(Value::as_i64).unwrap_or(1) as i32,
    );
    if (r1 - r0 + 1) as i64 * (c1 - c0 + 1) as i64 > 1_000_000 {
        return Err("local Pivot source range exceeds one million cells".to_string());
    }
    let mut cells = Vec::new();
    for row in r0..=r1 {
        for col in c0..=c1 {
            let value = match st
                .model
                .get_model()
                .get_cell_value_by_index(sheet, row, col)?
            {
                CellValue::Number(value) => json!(value),
                CellValue::String(value) => Value::String(value),
                CellValue::Boolean(value) => Value::Bool(value),
                CellValue::None => Value::Null,
            };
            if !value.is_null() {
                cells.push(json!({"r":row,"c":col,"v":value}));
            }
        }
    }
    let mut expanded = request.clone();
    expanded["source"] = json!({
        "kind":"worksheet",
        "cells":cells,
        "range":{"r0":r0,"c0":c0,"r1":r1,"c1":c1},
        "headerRow":range.get("headerRow").and_then(Value::as_i64).unwrap_or(r0 as i64),
        "firstDataRow":range.get("firstDataRow").and_then(Value::as_i64).unwrap_or((r0 + 1) as i64),
        "date1904":range.get("date1904").and_then(Value::as_bool).unwrap_or(false)
    });
    Ok(expanded)
}

fn write_local_pivot_result(
    st: &mut AppState,
    result: &Value,
    output: &Value,
) -> Result<Value, String> {
    let sheet = output.get("sheet").and_then(Value::as_u64).unwrap_or(0) as u32;
    let start_row = output.get("row").and_then(Value::as_i64).unwrap_or(1) as i32;
    let start_col = output.get("col").and_then(Value::as_i64).unwrap_or(1) as i32;
    let rows = result
        .get("result")
        .and_then(|value| value.get("rows"))
        .and_then(Value::as_array)
        .ok_or("local Pivot result has no rows")?;
    let columns = rows
        .iter()
        .filter_map(Value::as_array)
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    if rows.len().saturating_mul(columns) > 1_000_000 {
        return Err("local Pivot output exceeds one million cells".to_string());
    }
    if start_row < 1
        || start_col < 1
        || start_row + rows.len() as i32 - 1 > MAX_ROWS
        || start_col + columns as i32 - 1 > MAX_COLS
    {
        return Err("local Pivot output range is outside the worksheet".to_string());
    }
    st.model.pause_evaluation();
    let mut write_result = Ok(());
    'rows: for (row_offset, row) in rows.iter().enumerate() {
        let Some(values) = row.as_array() else {
            write_result = Err("local Pivot result row is not an array".to_string());
            break;
        };
        for (column_offset, value) in values.iter().enumerate() {
            let input = match value {
                Value::Null => String::new(),
                Value::Bool(value) => if *value { "TRUE" } else { "FALSE" }.to_string(),
                Value::Number(value) => value.to_string(),
                Value::String(value) => value.clone(),
                other => other.to_string(),
            };
            let row = start_row + row_offset as i32;
            let col = start_col + column_offset as i32;
            if let Err(error) = st.model.set_user_input(sheet, row, col, &input) {
                write_result = Err(error);
                break 'rows;
            }
            st.rich_text.remove(&(sheet, row, col));
            st.rich_text_xml.remove(&(sheet, row, col));
        }
    }
    st.model.resume_evaluation();
    st.model.evaluate();
    write_result?;
    Ok(json!({
        "sheet":sheet,"r0":start_row,"c0":start_col,
        "r1":start_row + rows.len() as i32 - 1,
        "c1":start_col + columns as i32 - 1
    }))
}

fn api_pivot_local_refresh(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = parse_body(body)?;
    let op = request
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or("preview");
    if op == "reset" {
        st.native_pivot_local_refresh_edits.clear();
        return ok_json(json!({"ok":true,"reset":true}));
    }
    let payload = request.get("request").unwrap_or(&request);
    let expanded = pivot_request_with_model_source(st, payload)?;
    if matches!(op, "preview" | "diagnose" | "validate") {
        return ok_json(pivot_local_refresh::diagnose_pivot_local(&expanded));
    }
    if op != "apply" && op != "refresh" {
        return Err(format!("bad local Pivot refresh op: {op}"));
    }
    let mut result =
        pivot_local_refresh::refresh_pivot_local(&expanded).map_err(|error| error.to_string())?;
    let package = expanded
        .get("package")
        .and_then(Value::as_object)
        .or_else(|| expanded.as_object());
    let package_requested = package.is_some_and(|package| {
        [
            "pivotTablePart",
            "pivotCacheDefinitionPart",
            "cacheRecordsPart",
            "cachePart",
        ]
        .iter()
        .any(|key| package.contains_key(*key))
    });
    let mut materialization = Value::Null;
    if package_requested {
        let mut parts = materialize_native_feature_parts(st)?;
        materialization =
            pivot_local_refresh::apply_local_refresh_ooxml(&mut parts, &expanded, &result)?;
        st.native_pivot_local_refresh_edits
            .push(json!({"request":expanded.clone(),"result":result.clone()}));
    }
    let output = request
        .get("output")
        .or_else(|| expanded.get("output"))
        .filter(|value| !value.is_null());
    let written = if let Some(output) = output {
        write_local_pivot_result(st, &result, output)?
    } else {
        Value::Null
    };
    if !package_requested && output.is_none() {
        return Err(
            "local Pivot apply requires an OOXML package target or worksheet output".into(),
        );
    }
    result["materialization"] = materialization;
    result["writtenRange"] = written;
    ok_json(result)
}

fn slicer_model_with_edits(st: &AppState) -> Result<Value, String> {
    let parts = materialize_native_feature_parts(st)?;
    let model = native_slicer_edit::inspect_native_slicers(&parts)?;
    let mut value = serde_json::to_value(model).map_err(|error| error.to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "edited".to_string(),
            Value::Bool(!st.native_slicer_edits.is_empty()),
        );
    }
    Ok(value)
}

fn api_slicers(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = if body.is_empty() {
        json!({"op":"list"})
    } else {
        parse_body(body)?
    };
    match request.get("op").and_then(Value::as_str).unwrap_or("list") {
        "list" => ok_json(slicer_model_with_edits(st)?),
        "update" => {
            let patch = request.get("patch").ok_or("missing slicer package patch")?;
            let mut parts = materialize_native_feature_parts(st)?;
            let before = parts.clone();
            native_slicer_edit::apply_slicer_package_edit(&mut parts, patch)?;
            if parts != before {
                st.native_slicer_edits.push(patch.clone());
            }
            ok_json(slicer_model_with_edits(st)?)
        }
        "reset" => {
            st.native_slicer_edits.clear();
            ok_json(slicer_model_with_edits(st)?)
        }
        other => Err(format!("bad slicer op: {other}")),
    }
}

fn parse_timeline_edit_request(
    value: &Value,
) -> Result<native_timeline_edit::TimelineEditRequest, String> {
    serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid typed Timeline patch: {error}"))
}

fn apply_timeline_edit_journal(
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    edits: &[Value],
) -> Result<(), String> {
    for value in edits {
        let request = parse_timeline_edit_request(value)?;
        native_timeline_edit::apply_timeline_edit(parts, &request)?;
    }
    Ok(())
}

fn timeline_model_with_edits(st: &AppState) -> Result<Value, String> {
    let parts = materialize_native_feature_parts(st)?;
    let model = native_timeline_edit::parse_timeline_model(&parts)?;
    let mut value = serde_json::to_value(model).map_err(|error| error.to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "edited".to_string(),
            Value::Bool(!st.native_timeline_edits.is_empty()),
        );
    }
    Ok(value)
}

fn api_timelines(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let request = if body.is_empty() {
        json!({"op":"list"})
    } else {
        parse_body(body)?
    };
    match request.get("op").and_then(Value::as_str).unwrap_or("list") {
        "list" => ok_json(timeline_model_with_edits(st)?),
        "update" => {
            let patch = request.get("patch").ok_or("missing Timeline edit patch")?;
            let typed = parse_timeline_edit_request(patch)?;
            let mut parts = materialize_native_feature_parts(st)?;
            let before = parts.clone();
            let current = native_timeline_edit::parse_timeline_model(&parts)?;
            let refresh_part = typed.cache.as_ref().and_then(|target| {
                current
                    .caches
                    .iter()
                    .find(|cache| cache.part == target.part)
                    .and_then(|cache| cache.state.pivot_cache_part.clone())
            });
            native_timeline_edit::apply_timeline_edit(&mut parts, &typed)?;
            if parts != before {
                st.native_timeline_edits.push(patch.clone());
                mark_native_pivot_cache_for_refresh(st, refresh_part.as_deref());
            }
            ok_json(timeline_model_with_edits(st)?)
        }
        "reset" => {
            st.native_timeline_edits.clear();
            ok_json(timeline_model_with_edits(st)?)
        }
        other => Err(format!("bad Timeline op: {other}")),
    }
}

// 字体家族：update_range_style 不支持 font.name，改读改写整体样式后 on_paste_styles
fn api_fontname(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let (r0, c0, r1, c1) = clamp_range(
        ji(&v, "r0")? as i32,
        ji(&v, "c0")? as i32,
        ji(&v, "r1")? as i32,
        ji(&v, "c1")? as i32,
    );
    let name = js(&v, "name")?;
    if (r1 - r0 + 1) as i64 * (c1 - c0 + 1) as i64 > 100_000 {
        return Err("range too large".into());
    }
    let mut styles: Vec<Vec<Style>> = Vec::new();
    for r in r0..=r1 {
        let mut line = Vec::new();
        for c in c0..=c1 {
            let mut s = st.model.get_cell_style(sheet, r, c)?;
            s.font.name = name.to_string();
            line.push(s);
        }
        styles.push(line);
    }
    st.model.set_selected_sheet(sheet)?;
    st.model.set_selected_cell(r0, c0)?;
    st.model.set_selected_range(r0, c0, r1, c1)?;
    st.model.on_paste_styles(&styles)?;
    ok_json(json!({}))
}

// 格式刷：源区域样式平铺到目标区域（Excel Format Painter 语义）
fn api_copystyle(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let (sr0, sc0, sr1, sc1) = clamp_range(
        ji(&v, "sr0")? as i32,
        ji(&v, "sc0")? as i32,
        ji(&v, "sr1")? as i32,
        ji(&v, "sc1")? as i32,
    );
    let dst_sheet = v
        .get("dstSheet")
        .and_then(|x| x.as_i64())
        .unwrap_or(sheet as i64) as u32;
    let (dr0, dc0, dr1, dc1) = clamp_range(
        ji(&v, "dr0")? as i32,
        ji(&v, "dc0")? as i32,
        ji(&v, "dr1")? as i32,
        ji(&v, "dc1")? as i32,
    );
    let sh = (sr1 - sr0 + 1) as usize;
    let sw = (sc1 - sc0 + 1) as usize;
    let dh = (dr1 - dr0 + 1) as usize;
    let dw = (dc1 - dc0 + 1) as usize;
    if dh * dw > 100_000 {
        return Err("range too large".into());
    }
    // 读源样式矩阵
    let mut src: Vec<Vec<Style>> = Vec::new();
    for r in 0..sh {
        let mut line = Vec::new();
        for c in 0..sw {
            line.push(
                st.model
                    .get_cell_style(sheet, sr0 + r as i32, sc0 + c as i32)?,
            );
        }
        src.push(line);
    }
    // 平铺到目标尺寸
    let mut styles: Vec<Vec<Style>> = Vec::new();
    for r in 0..dh {
        let mut line = Vec::new();
        for c in 0..dw {
            line.push(src[r % sh][c % sw].clone());
        }
        styles.push(line);
    }
    st.model.set_selected_sheet(dst_sheet)?;
    st.model.set_selected_cell(dr0, dc0)?;
    st.model.set_selected_range(dr0, dc0, dr1, dc1)?;
    st.model.on_paste_styles(&styles)?;
    ok_json(json!({}))
}

fn api_clear(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let rng = area(
        sheet,
        ji(&v, "r0")? as i32,
        ji(&v, "c0")? as i32,
        ji(&v, "r1")? as i32,
        ji(&v, "c1")? as i32,
    );
    match js(&v, "what")? {
        "contents" => st.model.range_clear_contents(&rng)?,
        "formatting" => st.model.range_clear_formatting(&rng)?,
        _ => st.model.range_clear_all(&rng)?,
    }
    ok_json(json!({}))
}

fn api_rows(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let row = ji(&v, "row")? as i32;
    let count = ji(&v, "count")?.max(1) as i32;
    let delete = match js(&v, "op")? {
        "insert" => {
            st.model.insert_rows(sheet, row, count)?;
            false
        }
        "delete" => {
            st.model.delete_rows(sheet, row, count)?;
            true
        }
        other => return Err(format!("bad rows op: {other}")),
    };
    transform_data_validations_for_structure(st, sheet, true, row, count, delete);
    ok_json(json!({}))
}

fn api_cols(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let col = ji(&v, "col")? as i32;
    let count = ji(&v, "count")?.max(1) as i32;
    let delete = match js(&v, "op")? {
        "insert" => {
            st.model.insert_columns(sheet, col, count)?;
            false
        }
        "delete" => {
            st.model.delete_columns(sheet, col, count)?;
            true
        }
        other => return Err(format!("bad cols op: {other}")),
    };
    transform_data_validations_for_structure(st, sheet, false, col, count, delete);
    ok_json(json!({}))
}

fn api_colwidth(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let c0 = ji(&v, "c0")? as i32;
    let c1 = ji(&v, "c1")? as i32;
    let width = v
        .get("width")
        .and_then(|x| x.as_f64())
        .ok_or("missing width")?;
    st.model.set_columns_width(sheet, c0, c1, width.max(0.0))?;
    ok_json(json!({}))
}

fn api_rowheight(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let r0 = ji(&v, "r0")? as i32;
    let r1 = ji(&v, "r1")? as i32;
    let height = v
        .get("height")
        .and_then(|x| x.as_f64())
        .ok_or("missing height")?;
    st.model.set_rows_height(sheet, r0, r1, height.max(0.0))?;
    ok_json(json!({}))
}

fn api_sheet(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    match js(&v, "op")? {
        "new" => st.model.new_sheet()?,
        "delete" => {
            let deleted = ji(&v, "sheet")? as u32;
            st.model.delete_sheet(deleted)?;
            remap_data_validation_sheet_keys(&mut st.worksheet_features, |sheet| {
                if sheet == deleted {
                    None
                } else if sheet > deleted {
                    Some(sheet - 1)
                } else {
                    Some(sheet)
                }
            });
            mark_all_data_validation_sheets_dirty(st);
        }
        "rename" => {
            let sheet = ji(&v, "sheet")? as u32;
            let new_name = js(&v, "name")?;
            let old_name = st
                .model
                .get_model()
                .workbook
                .worksheets
                .get(sheet as usize)
                .map(|worksheet| worksheet.name.clone())
                .ok_or("invalid sheet index")?;
            st.model.rename_sheet(sheet, new_name)?;
            rename_data_validation_sheet_references(st, &old_name, new_name);
        }
        "duplicate" => {
            let source = ji(&v, "sheet")? as u32;
            let duplicate = st.worksheet_features.data_validations.get(&source).cloned();
            st.model.duplicate_sheet(source)?;
            let new_index = source + 1;
            remap_data_validation_sheet_keys(&mut st.worksheet_features, |sheet| {
                Some(if sheet >= new_index { sheet + 1 } else { sheet })
            });
            if let Some(mut duplicate) = duplicate {
                for rule in &mut duplicate.rules {
                    rule.id = format!(
                        "dv-duplicate-{}",
                        DATA_VALIDATION_SEQ.fetch_add(1, Ordering::Relaxed)
                    );
                }
                st.worksheet_features
                    .data_validations
                    .insert(new_index, duplicate);
            }
            mark_all_data_validation_sheets_dirty(st);
        }
        "move" => {
            let from = ji(&v, "sheet")? as u32;
            let to = ji(&v, "to")? as u32;
            st.model.move_sheet(from, to)?;
            remap_data_validation_sheet_keys(&mut st.worksheet_features, |sheet| {
                Some(data_validation_index_after_move(sheet, from, to))
            });
            mark_all_data_validation_sheets_dirty(st);
        }
        other => return Err(format!("bad sheet op: {other}")),
    }
    let names = st.model.get_model().workbook.get_worksheet_names();
    ok_json(json!({ "sheets": names }))
}

fn api_autofill(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let (r0, c0, r1, c1) = clamp_range(
        ji(&v, "r0")? as i32,
        ji(&v, "c0")? as i32,
        ji(&v, "r1")? as i32,
        ji(&v, "c1")? as i32,
    );
    let rng = area(sheet, r0, c0, r1, c1);
    let mut target = (r0, c0, r1, c1);
    if let Ok(to_row) = ji(&v, "toRow") {
        target.0 = target.0.min(to_row as i32);
        target.2 = target.2.max(to_row as i32);
        if merged_ranges(st, sheet)?
            .iter()
            .any(|&(mr0, mc0, mr1, mc1)| {
                mr1 >= target.0 && mr0 <= target.2 && mc1 >= target.1 && mc0 <= target.3
            })
        {
            return Err("autofill across merged cells is not allowed".into());
        }
        st.model.auto_fill_rows(&rng, to_row as i32)?;
    } else if let Ok(to_col) = ji(&v, "toCol") {
        target.1 = target.1.min(to_col as i32);
        target.3 = target.3.max(to_col as i32);
        if merged_ranges(st, sheet)?
            .iter()
            .any(|&(mr0, mc0, mr1, mc1)| {
                mr1 >= target.0 && mr0 <= target.2 && mc1 >= target.1 && mc0 <= target.3
            })
        {
            return Err("autofill across merged cells is not allowed".into());
        }
        st.model.auto_fill_columns(&rng, to_col as i32)?;
    } else {
        return Err("need toRow or toCol".into());
    }
    ok_json(json!({}))
}

fn clipboard_border_css(
    st: &AppState,
    item: &Option<ironcalc::base::types::BorderItem>,
) -> Option<String> {
    let item = item.as_ref()?;
    let style_name = format!("{:?}", item.style).to_ascii_lowercase();
    let (width, line) = match style_name.as_str() {
        "double" => ("3px", "double"),
        "thick" => ("3px", "solid"),
        "medium" | "mediumdashed" | "mediumdashdot" | "mediumdashdotdot" => ("2px", "dashed"),
        "dotted" => ("1px", "dotted"),
        "slantdashdot" => ("1px", "dashed"),
        _ => ("1px", "solid"),
    };
    let color = st.model.resolve_color(&item.color);
    Some(format!(
        "{width} {line} {}",
        if color.is_empty() { "#000000" } else { &color }
    ))
}

fn clipboard_cell_css(st: &AppState, style: &Style) -> String {
    let mut css = Vec::new();
    if style.font.b {
        css.push("font-weight:bold".to_string());
    }
    if style.font.i {
        css.push("font-style:italic".to_string());
    }
    let mut decoration = Vec::new();
    if style.font.u {
        decoration.push("underline");
    }
    if style.font.strike {
        decoration.push("line-through");
    }
    if !decoration.is_empty() {
        css.push(format!("text-decoration:{}", decoration.join(" ")));
    }
    css.push(format!("font-size:{}pt", style.font.sz));
    if !style.font.name.is_empty() {
        css.push(format!(
            "font-family:'{}'",
            style.font.name.replace('\'', "\\'")
        ));
    }
    let font_color = st.model.resolve_color(&style.font.color);
    if !font_color.is_empty() {
        css.push(format!("color:{font_color}"));
    }
    let fill_color = st.model.resolve_color(&style.fill.color);
    if !fill_color.is_empty() {
        css.push(format!("background-color:{fill_color}"));
    }
    if let Some(alignment) = &style.alignment {
        css.push(format!("text-align:{}", alignment.horizontal));
        css.push(format!("vertical-align:{}", alignment.vertical));
        if alignment.wrap_text {
            css.push("white-space:normal".to_string());
        }
    }
    if style.num_fmt.to_ascii_lowercase() != "general" {
        css.push(format!(
            "mso-number-format:'{}'",
            style.num_fmt.replace('\'', "\\'")
        ));
    }
    for (side, item) in [
        ("top", &style.border.top),
        ("right", &style.border.right),
        ("bottom", &style.border.bottom),
        ("left", &style.border.left),
    ] {
        if let Some(border) = clipboard_border_css(st, item) {
            css.push(format!("border-{side}:{border}"));
        }
    }
    css.join(";")
}

fn clipboard_rich_html(st: &AppState, runs: &[RichTextRun]) -> String {
    let mut html = String::new();
    for run in runs {
        let mut css = Vec::new();
        if run.bold {
            css.push("font-weight:bold".to_string());
        }
        if run.italic {
            css.push("font-style:italic".to_string());
        }
        let mut decorations = Vec::new();
        if run.underline {
            decorations.push("underline");
        }
        if run.strike {
            decorations.push("line-through");
        }
        if !decorations.is_empty() {
            css.push(format!("text-decoration:{}", decorations.join(" ")));
        }
        if let Some(size) = run.size {
            css.push(format!("font-size:{size}pt"));
        }
        if let Some(font) = &run.font {
            css.push(format!("font-family:'{}'", font.replace('\'', "\\'")));
        }
        let color = st.model.resolve_color(&run.color);
        if !color.is_empty() {
            css.push(format!("color:{color}"));
        }
        html.push_str(&format!(
            "<span style=\"{}\">{}</span>",
            html_escape(&css.join(";")),
            html_escape(&run.text).replace('\n', "<br>")
        ));
    }
    html
}

fn clipboard_html(
    st: &AppState,
    sheet: u32,
    r0: i32,
    c0: i32,
    r1: i32,
    c1: i32,
) -> Result<String, String> {
    let merges = merged_ranges(st, sheet)?;
    let mut anchors = std::collections::HashMap::new();
    let mut covered = std::collections::HashSet::new();
    for &(mr0, mc0, mr1, mc1) in &merges {
        if mr0 < r0 || mc0 < c0 || mr1 > r1 || mc1 > c1 {
            continue;
        }
        anchors.insert((mr0, mc0), (mr1 - mr0 + 1, mc1 - mc0 + 1));
        for row in mr0..=mr1 {
            for col in mc0..=mc1 {
                if (row, col) != (mr0, mc0) {
                    covered.insert((row, col));
                }
            }
        }
    }
    let mut html = String::from(
        r#"<html xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:x="urn:schemas-microsoft-com:office:excel"><head><meta charset="utf-8"><meta name="ProgId" content="Excel.Sheet"></head><body><table data-unicell-version="1" cellspacing="0" cellpadding="0"><colgroup>"#,
    );
    for col in c0..=c1 {
        let width = st.model.get_column_width(sheet, col).unwrap_or(64.0);
        html.push_str(&format!("<col style=\"width:{width}px\">"));
    }
    html.push_str("</colgroup>");
    for row in r0..=r1 {
        let height = st.model.get_row_height(sheet, row).unwrap_or(20.0);
        html.push_str(&format!("<tr style=\"height:{height}px\">"));
        for col in c0..=c1 {
            if covered.contains(&(row, col)) {
                continue;
            }
            let style = st.model.get_cell_style(sheet, row, col)?;
            let content = st.model.get_cell_content(sheet, row, col)?;
            let formatted = st.model.get_formatted_cell_value(sheet, row, col)?;
            let (rowspan, colspan) = anchors.get(&(row, col)).copied().unwrap_or((1, 1));
            let mut attrs = format!(
                " style=\"{}\"",
                html_escape(&clipboard_cell_css(st, &style))
            );
            if rowspan > 1 {
                attrs.push_str(&format!(" rowspan=\"{rowspan}\""));
            }
            if colspan > 1 {
                attrs.push_str(&format!(" colspan=\"{colspan}\""));
            }
            if content.starts_with('=') {
                attrs.push_str(&format!(" x:fmla=\"{}\"", html_escape(&content)));
            }
            let cell_html = st
                .rich_text
                .get(&(sheet, row, col))
                .map(|runs| clipboard_rich_html(st, runs))
                .unwrap_or_else(|| html_escape(&formatted).replace('\n', "<br>"));
            html.push_str(&format!("<td{attrs}>{cell_html}</td>"));
        }
        html.push_str("</tr>");
    }
    html.push_str("</table></body></html>");
    Ok(html)
}

fn clipboard_relative_areas(sqref: &str, r0: i32, c0: i32, r1: i32, c1: i32) -> Vec<Value> {
    let mut areas = Vec::new();
    for token in sqref.split_whitespace() {
        let (first, second) = token.split_once(':').unwrap_or((token, token));
        let (Some((ar, ac)), Some((br, bc))) = (parse_a1(first), parse_a1(second)) else {
            continue;
        };
        let (ar0, ac0, ar1, ac1) = (ar.min(br), ac.min(bc), ar.max(br), ac.max(bc));
        let (ir0, ic0, ir1, ic1) = (ar0.max(r0), ac0.max(c0), ar1.min(r1), ac1.min(c1));
        if ir0 <= ir1 && ic0 <= ic1 {
            areas.push(json!({
                "r0": ir0-r0, "c0": ic0-c0,
                "r1": ir1-r0, "c1": ic1-c0
            }));
        }
    }
    areas
}

fn clipboard_payload(
    st: &AppState,
    clip: Value,
    sheet: u32,
    r0: i32,
    c0: i32,
    r1: i32,
    c1: i32,
    display: &str,
    raw: &str,
) -> Result<Value, String> {
    // Paste Special > Values must use the evaluated scalar, not the formatted display string.
    // Using `display` here silently rounded numbers (and converted dates/percentages to labels)
    // whenever the source cell had a number format.
    let mut values = Vec::with_capacity((r1 - r0 + 1).max(0) as usize);
    for row in r0..=r1 {
        let mut value_row = Vec::with_capacity((c1 - c0 + 1).max(0) as usize);
        for col in c0..=c1 {
            value_row.push(
                match st
                    .model
                    .get_model()
                    .get_cell_value_by_index(sheet, row, col)?
                {
                    CellValue::Number(value) => json!(value),
                    CellValue::String(value) => Value::String(value),
                    CellValue::Boolean(value) => Value::Bool(value),
                    CellValue::None => Value::Null,
                },
            );
        }
        values.push(Value::Array(value_row));
    }
    let rich_text = st
        .rich_text
        .iter()
        .filter(|((s, r, c), _)| *s == sheet && *r >= r0 && *r <= r1 && *c >= c0 && *c <= c1)
        .map(|((_, r, c), runs)| json!({"r":r-r0,"c":c-c0,"runs":runs}))
        .collect::<Vec<_>>();
    let merges = merged_ranges(st, sheet)
        .unwrap_or_default()
        .into_iter()
        .filter(|(ar0, ac0, ar1, ac1)| *ar0 >= r0 && *ac0 >= c0 && *ar1 <= r1 && *ac1 <= c1)
        .map(|(ar0, ac0, ar1, ac1)| {
            json!({
                "r0":ar0-r0,"c0":ac0-c0,"r1":ar1-r0,"c1":ac1-c0
            })
        })
        .collect::<Vec<_>>();
    let validations = st
        .worksheet_features
        .data_validations
        .get(&sheet)
        .map(|transport| {
            transport
                .rules
                .iter()
                .filter_map(|rule| {
                    let areas = clipboard_relative_areas(&rule.sqref, r0, c0, r1, c1);
                    (!areas.is_empty()).then(|| json!({"rule":rule,"areas":areas}))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let objects = st
        .objects
        .get(&sheet)
        .into_iter()
        .flatten()
        .filter_map(|object| {
            let row = object.get("r").and_then(Value::as_i64)? as i32;
            let col = object.get("c").and_then(Value::as_i64)? as i32;
            (row >= r0 && row <= r1 && col >= c0 && col <= c1)
                .then(|| json!({"r":row-r0,"c":col-c0,"object":object}))
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "version":1,
        "kind":"unicell-range",
        "originRow":r0,
        "originCol":c0,
        "clip":clip,
        "display":display,
        "raw":raw,
        "values":values,
        "height":r1-r0+1,
        "width":c1-c0+1,
        "richText":rich_text,
        "merges":merges,
        "validations":validations,
        "objects":objects
    }))
}

// 复制：走引擎剪贴板（完整保留公式/样式/溢出数组信息），同时生成显示值 TSV 供系统剪贴板
fn api_copy(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let (r0, c0, r1, c1) = clamp_range(
        ji(&v, "r0")? as i32,
        ji(&v, "c0")? as i32,
        ji(&v, "r1")? as i32,
        ji(&v, "c1")? as i32,
    );
    if (r1 - r0) > 10000 || (c1 - c0) > 1000 {
        return Err("copy range too large".into());
    }
    // 引擎剪贴板抓取当前选区 → 先同步选区（选中单元格须在区域角上）
    st.model.set_selected_sheet(sheet)?;
    st.model.set_selected_cell(r0, c0)?;
    st.model.set_selected_range(r0, c0, r1, c1)?;
    let clip = st.model.copy_to_clipboard()?;
    let clip_json = serde_json::to_value(&clip).map_err(|e| format!("clip: {e}"))?;
    let mut tsv = String::new();
    let mut display = String::new();
    for r in r0..=r1 {
        if r > r0 {
            tsv.push('\n');
            display.push('\n');
        }
        for c in c0..=c1 {
            if c > c0 {
                tsv.push('\t');
                display.push('\t');
            }
            tsv.push_str(&st.model.get_cell_content(sheet, r, c)?);
            display.push_str(&st.model.get_formatted_cell_value(sheet, r, c)?);
        }
    }
    st.clip_tsv = Some(display.clone());
    st.clip_engine = Some(clip_json.clone());
    let html = clipboard_html(st, sheet, r0, c0, r1, c1)?;
    let payload = clipboard_payload(st, clip_json, sheet, r0, c0, r1, c1, &display, &tsv)?;
    ok_json(json!({ "tsv": display, "raw": tsv, "html": html, "unicell": payload }))
}

fn transpose_tsv(text: &str) -> String {
    let rows = text
        .split('\n')
        .map(|line| {
            line.trim_end_matches('\r')
                .split('\t')
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut output = Vec::with_capacity(width);
    for column in 0..width {
        output.push(
            rows.iter()
                .map(|row| row.get(column).cloned().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\t"),
        );
    }
    output.join("\n")
}

fn transpose_clipboard_value(clip: &Value) -> Result<Value, String> {
    let range = clip
        .get("range")
        .and_then(Value::as_array)
        .ok_or("clipboard payload has no range")?;
    if range.len() != 4 {
        return Err("clipboard range must have four coordinates".into());
    }
    let r0 = range[0].as_i64().ok_or("bad clipboard row")? as i32;
    let c0 = range[1].as_i64().ok_or("bad clipboard column")? as i32;
    let r1 = range[2].as_i64().ok_or("bad clipboard row")? as i32;
    let c1 = range[3].as_i64().ok_or("bad clipboard column")? as i32;
    let mut data = serde_json::Map::new();
    for (row_key, row_value) in clip
        .get("data")
        .and_then(Value::as_object)
        .ok_or("clipboard payload has no data")?
    {
        let source_row = row_key
            .parse::<i32>()
            .map_err(|_| "bad clipboard row key")?;
        for (column_key, cell) in row_value
            .as_object()
            .ok_or("clipboard row is not an object")?
        {
            let source_col = column_key
                .parse::<i32>()
                .map_err(|_| "bad clipboard column key")?;
            let target_row = r0 + source_col - c0;
            let target_col = c0 + source_row - r0;
            data.entry(target_row.to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .expect("new clipboard row is an object")
                .insert(target_col.to_string(), cell.clone());
        }
    }
    let mut transposed = clip.clone();
    transposed["range"] = json!([r0, c0, r0 + (c1 - c0), c0 + (r1 - r0)]);
    transposed["data"] = Value::Object(data);
    Ok(transposed)
}

fn clipboard_style_matrix(clip: &Value) -> Result<Vec<Vec<Style>>, String> {
    let range = clip
        .get("range")
        .and_then(Value::as_array)
        .ok_or("clipboard payload has no range")?;
    if range.len() != 4 {
        return Err("clipboard range must have four coordinates".into());
    }
    let r0 = range[0].as_i64().ok_or("bad clipboard row")? as i32;
    let c0 = range[1].as_i64().ok_or("bad clipboard column")? as i32;
    let r1 = range[2].as_i64().ok_or("bad clipboard row")? as i32;
    let c1 = range[3].as_i64().ok_or("bad clipboard column")? as i32;
    let mut styles = vec![vec![Style::default(); (c1 - c0 + 1) as usize]; (r1 - r0 + 1) as usize];
    for (row_key, row_value) in clip
        .get("data")
        .and_then(Value::as_object)
        .ok_or("clipboard payload has no data")?
    {
        let source_row = row_key
            .parse::<i32>()
            .map_err(|_| "bad clipboard row key")?;
        for (column_key, cell) in row_value
            .as_object()
            .ok_or("clipboard row is not an object")?
        {
            let source_col = column_key
                .parse::<i32>()
                .map_err(|_| "bad clipboard column key")?;
            if !(r0..=r1).contains(&source_row) || !(c0..=c1).contains(&source_col) {
                continue;
            }
            styles[(source_row - r0) as usize][(source_col - c0) as usize] =
                serde_json::from_value(
                    cell.get("style")
                        .cloned()
                        .ok_or("clipboard cell has no style")?,
                )
                .map_err(|error| format!("clipboard style: {error}"))?;
        }
    }
    Ok(styles)
}

fn shift_copied_formula_references(formula: &str, row_delta: i32, col_delta: i32) -> String {
    let bytes = formula.as_bytes();
    let mut result = String::with_capacity(formula.len());
    let mut index = 0usize;
    let mut in_string = false;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            result.push('"');
            if in_string && bytes.get(index + 1) == Some(&b'"') {
                result.push('"');
                index += 2;
                continue;
            }
            in_string = !in_string;
            index += 1;
            continue;
        }
        if in_string {
            let ch = formula[index..].chars().next().unwrap();
            result.push(ch);
            index += ch.len_utf8();
            continue;
        }
        let Some((end, mut reference)) = parse_formula_cell_reference(formula, index) else {
            let ch = formula[index..].chars().next().unwrap();
            result.push(ch);
            index += ch.len_utf8();
            continue;
        };
        if let Some(row) = reference.row {
            if !reference.row_absolute {
                reference.row = row
                    .checked_add(row_delta)
                    .filter(|value| (1..=MAX_ROWS).contains(value));
            }
        }
        if let Some(column) = reference.column {
            if !reference.column_absolute {
                reference.column = column
                    .checked_add(col_delta)
                    .filter(|value| (1..=MAX_COLS).contains(value));
            }
        }
        if reference.row.is_none() || reference.column.is_none() {
            result.push_str("#REF!");
        } else {
            result.push_str(&format_data_validation_reference(reference));
        }
        index = end;
    }
    result
}

fn paste_external_text(
    st: &mut AppState,
    sheet: u32,
    row: i32,
    col: i32,
    text: &str,
) -> Result<(usize, i32), String> {
    let merged = merged_ranges(st, sheet)?;
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect();
    let mut resolved = Vec::new();
    let mut targets = std::collections::HashSet::new();
    let mut max_c = 0;
    for (dr, line) in lines.iter().enumerate() {
        if lines.len() > 1 && dr == lines.len() - 1 && line.is_empty() {
            continue;
        }
        for (dc, field) in line.split('\t').enumerate() {
            let target_row = row + dr as i32;
            let target_col = col + dc as i32;
            if target_row > MAX_ROWS || target_col > MAX_COLS {
                continue;
            }
            let target = merged_anchor_in(&merged, target_row, target_col);
            if !targets.insert(target) {
                return Err("cannot paste multiple cells into one merged cell".into());
            }
            resolved.push((target.0, target.1, field));
            max_c = max_c.max(dc as i32 + 1);
        }
    }
    st.model.pause_evaluation();
    let mut write_result = Ok(());
    for (target_row, target_col, field) in resolved {
        if let Err(error) = st
            .model
            .set_user_input(sheet, target_row, target_col, field)
        {
            write_result = Err(error);
            break;
        }
        st.rich_text.remove(&(sheet, target_row, target_col));
        st.rich_text_xml.remove(&(sheet, target_row, target_col));
    }
    st.model.resume_evaluation();
    st.model.evaluate();
    write_result?;
    Ok((lines.len(), max_c))
}

fn transpose_relative_entry(entry: &Value) -> Value {
    let mut result = entry.clone();
    if let (Some(row), Some(col)) = (
        entry.get("r").and_then(Value::as_i64),
        entry.get("c").and_then(Value::as_i64),
    ) {
        result["r"] = json!(col);
        result["c"] = json!(row);
    }
    if let (Some(r0), Some(c0), Some(r1), Some(c1)) = (
        entry.get("r0").and_then(Value::as_i64),
        entry.get("c0").and_then(Value::as_i64),
        entry.get("r1").and_then(Value::as_i64),
        entry.get("c1").and_then(Value::as_i64),
    ) {
        result["r0"] = json!(c0);
        result["c0"] = json!(r0);
        result["r1"] = json!(c1);
        result["c1"] = json!(r1);
    }
    result
}

fn paste_clipboard_value_matrix(
    st: &mut AppState,
    sheet: u32,
    row: i32,
    col: i32,
    matrix: &Value,
    transpose: bool,
) -> Result<(), String> {
    let rows = matrix
        .as_array()
        .ok_or("clipboard values must be a row array")?;
    let merged = merged_ranges(st, sheet)?;
    let mut targets = std::collections::HashSet::new();
    let mut writes = Vec::new();
    for (source_row, values) in rows.iter().enumerate() {
        let values = values
            .as_array()
            .ok_or("clipboard values rows must be arrays")?;
        for (source_col, value) in values.iter().enumerate() {
            if !(value.is_null() || value.is_boolean() || value.is_number() || value.is_string()) {
                return Err("clipboard values must contain only scalar cells".into());
            }
            let (target_row, target_col) = if transpose {
                (row + source_col as i32, col + source_row as i32)
            } else {
                (row + source_row as i32, col + source_col as i32)
            };
            if target_row > MAX_ROWS || target_col > MAX_COLS {
                continue;
            }
            let target = merged_anchor_in(&merged, target_row, target_col);
            if !targets.insert(target) {
                return Err("cannot paste multiple cells into one merged cell".into());
            }
            writes.push((target.0, target.1, value));
        }
    }

    st.model.pause_evaluation();
    let mut write_result = Ok(());
    for (target_row, target_col, value) in writes {
        let result = match value {
            Value::Null => st.model.set_user_input(sheet, target_row, target_col, ""),
            Value::Bool(value) => st.model.set_user_input(
                sheet,
                target_row,
                target_col,
                if *value { "TRUE" } else { "FALSE" },
            ),
            Value::Number(value) => {
                st.model
                    .set_user_input(sheet, target_row, target_col, &value.to_string())
            }
            Value::String(value) => {
                // A calculated string beginning with '=' remains a string under Excel's
                // Paste Values semantics; the normal input path would reinterpret it as a formula.
                st.model
                    .set_rich_text_plain_value(sheet, target_row, target_col, value)
            }
            _ => unreachable!("scalar values validated above"),
        };
        if let Err(error) = result {
            write_result = Err(error);
            break;
        }
        st.rich_text.remove(&(sheet, target_row, target_col));
        st.rich_text_xml.remove(&(sheet, target_row, target_col));
    }
    if st.calculation_mode == CalculationMode::Automatic {
        st.model.resume_evaluation();
        st.model.evaluate();
    }
    write_result
}

fn apply_unicell_clipboard_sidecars(
    st: &mut AppState,
    sheet: u32,
    row: i32,
    col: i32,
    payload: &Value,
    transpose: bool,
) -> Result<(), String> {
    let height = payload.get("height").and_then(Value::as_i64).unwrap_or(1) as i32;
    let width = payload.get("width").and_then(Value::as_i64).unwrap_or(1) as i32;
    let (target_height, target_width) = if transpose {
        (width, height)
    } else {
        (height, width)
    };
    let target_r1 = (row + target_height - 1).min(MAX_ROWS);
    let target_c1 = (col + target_width - 1).min(MAX_COLS);
    st.rich_text.retain(|(s, r, c), _| {
        !(*s == sheet && *r >= row && *r <= target_r1 && *c >= col && *c <= target_c1)
    });
    st.rich_text_xml.retain(|(s, r, c), _| {
        !(*s == sheet && *r >= row && *r <= target_r1 && *c >= col && *c <= target_c1)
    });
    for raw_entry in payload
        .get("richText")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let entry = if transpose {
            transpose_relative_entry(raw_entry)
        } else {
            raw_entry.clone()
        };
        let target_row = row + entry.get("r").and_then(Value::as_i64).unwrap_or(0) as i32;
        let target_col = col + entry.get("c").and_then(Value::as_i64).unwrap_or(0) as i32;
        let runs: Vec<RichTextRun> =
            serde_json::from_value(entry.get("runs").cloned().unwrap_or_else(|| json!([])))
                .map_err(|error| format!("clipboard rich text: {error}"))?;
        let content = runs.iter().map(|run| run.text.as_str()).collect::<String>();
        st.model
            .set_rich_text_plain_value(sheet, target_row, target_col, &content)?;
        let xml = build_rich_shared_item_xml(None, &[], &runs)?;
        st.rich_text.insert((sheet, target_row, target_col), runs);
        st.rich_text_xml
            .insert((sheet, target_row, target_col), xml);
    }

    let copied_merges = payload
        .get("merges")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !copied_merges.is_empty() {
        st.model
            .unmerge_cells_range(sheet, row, col, target_r1, target_c1)?;
        for raw_merge in copied_merges {
            let merge = if transpose {
                transpose_relative_entry(&raw_merge)
            } else {
                raw_merge
            };
            let mr0 = row + ji(&merge, "r0")? as i32;
            let mc0 = col + ji(&merge, "c0")? as i32;
            let mr1 = row + ji(&merge, "r1")? as i32;
            let mc1 = col + ji(&merge, "c1")? as i32;
            st.model.merge_cells_range(sheet, mr0, mc0, mr1, mc1)?;
        }
    }

    let origin_row = payload
        .get("originRow")
        .and_then(Value::as_i64)
        .unwrap_or(1) as i32;
    let origin_col = payload
        .get("originCol")
        .and_then(Value::as_i64)
        .unwrap_or(1) as i32;
    let validation_entries = payload
        .get("validations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !validation_entries.is_empty() {
        let transport = st
            .worksheet_features
            .data_validations
            .entry(sheet)
            .or_default();
        for entry in validation_entries {
            let mut rule: DataValidationRule = serde_json::from_value(
                entry
                    .get("rule")
                    .cloned()
                    .ok_or("clipboard validation has no rule")?,
            )
            .map_err(|error| format!("clipboard validation: {error}"))?;
            let mut refs = Vec::new();
            for raw_area in entry
                .get("areas")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let area = if transpose {
                    transpose_relative_entry(raw_area)
                } else {
                    raw_area.clone()
                };
                let ar0 = row + ji(&area, "r0")? as i32;
                let ac0 = col + ji(&area, "c0")? as i32;
                let ar1 = row + ji(&area, "r1")? as i32;
                let ac1 = col + ji(&area, "c1")? as i32;
                refs.push(if ar0 == ar1 && ac0 == ac1 {
                    cell_ref_str(ar0, ac0)
                } else {
                    format!("{}:{}", cell_ref_str(ar0, ac0), cell_ref_str(ar1, ac1))
                });
            }
            if refs.is_empty() {
                continue;
            }
            rule.id = format!(
                "dv-paste-{}",
                DATA_VALIDATION_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            rule.sqref = refs.join(" ");
            rule.raw_xml = None;
            let row_delta = row - origin_row;
            let col_delta = col - origin_col;
            rule.formula1 = rule
                .formula1
                .map(|formula| shift_copied_formula_references(&formula, row_delta, col_delta));
            rule.formula2 = rule
                .formula2
                .map(|formula| shift_copied_formula_references(&formula, row_delta, col_delta));
            transport.rules.push(rule);
        }
        st.worksheet_features.data_validation_dirty.insert(sheet);
    }

    for raw_entry in payload
        .get("objects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let entry = if transpose {
            transpose_relative_entry(raw_entry)
        } else {
            raw_entry.clone()
        };
        let mut object = entry
            .get("object")
            .cloned()
            .ok_or("clipboard object has no payload")?;
        let target_row = row + entry.get("r").and_then(Value::as_i64).unwrap_or(0) as i32;
        let target_col = col + entry.get("c").and_then(Value::as_i64).unwrap_or(0) as i32;
        object["sheet"] = json!(sheet);
        object["r"] = json!(target_row);
        object["c"] = json!(target_col);
        object["id"] = json!(format!(
            "clipboard-{}",
            CLIPBOARD_OBJECT_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        st.objects.entry(sheet).or_default().push(object);
    }
    Ok(())
}

fn paste_unicell_payload(
    st: &mut AppState,
    sheet: u32,
    row: i32,
    col: i32,
    payload: &Value,
    special: &str,
) -> Result<(i32, i32), String> {
    if payload.get("kind").and_then(Value::as_str) != Some("unicell-range")
        || payload.get("version").and_then(Value::as_i64) != Some(1)
    {
        return Err("unsupported UniCell clipboard payload".into());
    }
    let transpose = special.starts_with("transpose");
    let mode = special.strip_prefix("transpose-").unwrap_or(special);
    let height = payload.get("height").and_then(Value::as_i64).unwrap_or(1) as i32;
    let width = payload.get("width").and_then(Value::as_i64).unwrap_or(1) as i32;
    let (rows, cols) = if transpose {
        (width, height)
    } else {
        (height, width)
    };
    match mode {
        "values" | "formulas" => {
            if mode == "values" {
                if let Some(values) = payload.get("values") {
                    paste_clipboard_value_matrix(st, sheet, row, col, values, transpose)?;
                    return Ok((rows, cols));
                }
            }
            // Backward-compatible fallback for clipboard payloads produced before the exact
            // evaluated-value matrix was added, and for external HTML with no custom payload.
            let key = if mode == "values" { "display" } else { "raw" };
            let mut text = payload
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if transpose {
                text = transpose_tsv(&text);
            }
            paste_external_text(st, sheet, row, col, &text)?;
        }
        "formats" => {
            let mut clip = payload
                .get("clip")
                .cloned()
                .ok_or("clipboard payload has no engine data")?;
            if transpose {
                clip = transpose_clipboard_value(&clip)?;
            }
            let styles = clipboard_style_matrix(&clip)?;
            st.model.set_selected_sheet(sheet)?;
            st.model.set_selected_cell(row, col)?;
            st.model.set_selected_range(row, col, row, col)?;
            st.model.on_paste_styles(&styles)?;
        }
        "all" | "" => {
            let mut clip = payload
                .get("clip")
                .cloned()
                .ok_or("clipboard payload has no engine data")?;
            if transpose {
                clip = transpose_clipboard_value(&clip)?;
            }
            let range = clip
                .get("range")
                .and_then(Value::as_array)
                .ok_or("clipboard payload has no range")?;
            if range.len() != 4 {
                return Err("clipboard range must have four coordinates".into());
            }
            let source_range = (
                range[0].as_i64().ok_or("bad clipboard row")? as i32,
                range[1].as_i64().ok_or("bad clipboard column")? as i32,
                range[2].as_i64().ok_or("bad clipboard row")? as i32,
                range[3].as_i64().ok_or("bad clipboard column")? as i32,
            );
            let data: ClipboardData = serde_json::from_value(
                clip.get("data")
                    .cloned()
                    .ok_or("clipboard payload has no data")?,
            )
            .map_err(|error| format!("clipboard data: {error}"))?;
            st.model.set_selected_sheet(sheet)?;
            st.model.set_selected_cell(row, col)?;
            st.model.set_selected_range(row, col, row, col)?;
            st.model
                .paste_from_external_clipboard(source_range, &data)?;
            apply_unicell_clipboard_sidecars(st, sheet, row, col, payload, transpose)?;
        }
        other => return Err(format!("unsupported paste-special mode: {other}")),
    }
    Ok((rows, cols))
}

type CellRect = (i32, i32, i32, i32);

fn rects_intersect(left: CellRect, right: CellRect) -> bool {
    left.2 >= right.0 && left.0 <= right.2 && left.3 >= right.1 && left.1 <= right.3
}

fn rect_contains(outer: CellRect, inner: CellRect) -> bool {
    inner.0 >= outer.0 && inner.2 <= outer.2 && inner.1 >= outer.1 && inner.3 <= outer.3
}

fn parse_sqref_rectangles(sqref: &str) -> Result<Vec<CellRect>, String> {
    sqref
        .split_whitespace()
        .map(|part| {
            let (first, second) = part.split_once(':').unwrap_or((part, part));
            let (first_row, first_col) =
                parse_a1(first).ok_or_else(|| format!("invalid range reference '{part}'"))?;
            let (second_row, second_col) =
                parse_a1(second).ok_or_else(|| format!("invalid range reference '{part}'"))?;
            Ok((
                first_row.min(second_row),
                first_col.min(second_col),
                first_row.max(second_row),
                first_col.max(second_col),
            ))
        })
        .collect()
}

fn format_sqref_rectangles(rectangles: &[CellRect]) -> String {
    rectangles
        .iter()
        .map(|&(r0, c0, r1, c1)| {
            if r0 == r1 && c0 == c1 {
                cell_ref_str(r0, c0)
            } else {
                format!("{}:{}", cell_ref_str(r0, c0), cell_ref_str(r1, c1))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn classify_rule_range(rectangles: &[CellRect], selection: CellRect) -> Result<bool, String> {
    let mut contained = false;
    let mut disjoint = false;
    for &rectangle in rectangles {
        if !rects_intersect(rectangle, selection) {
            disjoint = true;
        } else if rect_contains(selection, rectangle) {
            contained = true;
        } else {
            return Err("selection partially intersects a rule range".to_string());
        }
    }
    if contained && disjoint {
        return Err("selection intersects only part of a multi-area rule".to_string());
    }
    Ok(contained)
}

fn shift_rectangles(
    rectangles: &[CellRect],
    row_delta: i32,
    column_delta: i32,
) -> Result<Vec<CellRect>, String> {
    rectangles
        .iter()
        .map(|&(r0, c0, r1, c1)| {
            let shifted = (
                r0 + row_delta,
                c0 + column_delta,
                r1 + row_delta,
                c1 + column_delta,
            );
            if shifted.0 < 1 || shifted.1 < 1 || shifted.2 > MAX_ROWS || shifted.3 > MAX_COLS {
                Err("cut destination is outside worksheet bounds".to_string())
            } else {
                Ok(shifted)
            }
        })
        .collect()
}

fn formula_contains_a1_reference(formula: &str) -> bool {
    (0..formula.len()).any(|index| parse_formula_cell_reference(formula, index).is_some())
}

fn move_validation_formula(
    model: &mut UserModel,
    formula: Option<String>,
    source_anchor: (i32, i32),
    target_anchor: (i32, i32),
    source_sheet: u32,
    target_sheet: u32,
    source_area: &Area,
) -> Result<Option<String>, String> {
    let Some(formula) = formula else {
        return Ok(None);
    };
    if !formula_contains_a1_reference(&formula) {
        return Ok(Some(formula));
    }
    let had_equals = formula.trim_start().starts_with('=');
    let value = if had_equals {
        formula.clone()
    } else {
        format!("={formula}")
    };
    let moved = model.move_value_to_area(
        &value,
        &CellReferenceIndex {
            sheet: source_sheet,
            row: source_anchor.0,
            column: source_anchor.1,
        },
        &CellReferenceIndex {
            sheet: target_sheet,
            row: target_anchor.0,
            column: target_anchor.1,
        },
        source_area,
    )?;
    Ok(Some(if had_equals {
        moved
    } else {
        moved.strip_prefix('=').unwrap_or(&moved).to_string()
    }))
}

#[derive(Debug)]
struct CutSidecarMovePlan {
    source_merges: Vec<CellRect>,
    rich_text: std::collections::HashMap<(u32, i32, i32), Vec<RichTextRun>>,
    rich_text_xml: std::collections::HashMap<(u32, i32, i32), String>,
    objects: std::collections::HashMap<u32, Vec<Value>>,
    data_validations: std::collections::HashMap<u32, DataValidationSheet>,
    data_validation_dirty: std::collections::HashSet<u32>,
}

fn cell_origin_pixels(model: &UserModel, sheet: u32, row: i32, column: i32) -> (f64, f64) {
    let x = (1..column)
        .map(|index| model.get_column_width(sheet, index).unwrap_or(100.0))
        .sum();
    let y = (1..row)
        .map(|index| model.get_row_height(sheet, index).unwrap_or(21.0))
        .sum();
    (x, y)
}

fn reject_table_intersections(
    st: &AppState,
    source_sheet: u32,
    source: CellRect,
    target_sheet: u32,
    target: CellRect,
) -> Result<(), String> {
    let source_name = st
        .model
        .get_model()
        .workbook
        .worksheet(source_sheet)?
        .get_name();
    let target_name = st
        .model
        .get_model()
        .workbook
        .worksheet(target_sheet)?
        .get_name();
    for table in st.model.get_tables().values() {
        let Some((first, second)) = table.reference.split_once(':') else {
            return Err(format!("table {} has an invalid range", table.display_name));
        };
        let (Some((r0, c0)), Some((r1, c1))) = (parse_a1(first), parse_a1(second)) else {
            return Err(format!("table {} has an invalid range", table.display_name));
        };
        let table_range = (r0.min(r1), c0.min(c1), r0.max(r1), c0.max(c1));
        if (table.sheet_name == source_name && rects_intersect(table_range, source))
            || (table.sheet_name == target_name && rects_intersect(table_range, target))
        {
            return Err(format!(
                "Cannot cut through Excel table '{}'; move or resize the table first",
                table.display_name
            ));
        }
    }
    Ok(())
}

fn x14_rule_rectangles(st: &AppState, sheet: u32) -> Result<Vec<CellRect>, String> {
    let Some(snapshot) = st.source_ooxml.as_ref() else {
        return Ok(Vec::new());
    };
    let sheet_paths = snapshot_workbook_sheet_paths(snapshot);
    let Some(path) = sheet_paths.get(sheet as usize) else {
        return Ok(Vec::new());
    };
    let Some(bytes) = snapshot.parts.get(path) else {
        return Ok(Vec::new());
    };
    let xml =
        std::str::from_utf8(bytes).map_err(|error| format!("worksheet extension utf8: {error}"))?;
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| format!("worksheet extension XML: {error}"))?;
    let mut rectangles = Vec::new();
    for rule in document.descendants().filter(|node| {
        if !node.is_element()
            || !matches!(
                node.tag_name().name(),
                "conditionalFormatting" | "dataValidation"
            )
        {
            return false;
        }
        node.tag_name()
            .namespace()
            .is_some_and(|namespace| namespace.contains("spreadsheetml/2009/9/main"))
    }) {
        if let Some(sqref) = rule.attribute("sqref") {
            rectangles.extend(parse_sqref_rectangles(sqref)?);
        }
        for sqref in rule.descendants().filter(|node| {
            node.is_element() && node.tag_name().name().eq_ignore_ascii_case("sqref")
        }) {
            if let Some(value) = sqref
                .text()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                rectangles.extend(parse_sqref_rectangles(value)?);
            }
        }
    }
    Ok(rectangles)
}

fn reject_cross_sheet_x14_rule_intersections(
    st: &AppState,
    source_sheet: u32,
    source: CellRect,
    target_sheet: u32,
    target: CellRect,
) -> Result<(), String> {
    if source_sheet == target_sheet {
        return Ok(());
    }
    for (sheet, selection, role) in [
        (source_sheet, source, "source"),
        (target_sheet, target, "destination"),
    ] {
        if x14_rule_rectangles(st, sheet)?
            .into_iter()
            .any(|rectangle| rects_intersect(rectangle, selection))
        {
            return Err(format!(
                "Cannot cross-sheet cut through preserve-only x14 conditional-formatting/data-validation {role} range"
            ));
        }
    }
    Ok(())
}

fn prepare_cut_sidecar_move(
    st: &mut AppState,
    source_sheet: u32,
    source: CellRect,
    target_sheet: u32,
    target: CellRect,
) -> Result<CutSidecarMovePlan, String> {
    reject_table_intersections(st, source_sheet, source, target_sheet, target)?;
    reject_cross_sheet_x14_rule_intersections(st, source_sheet, source, target_sheet, target)?;

    let source_merges = merged_ranges(st, source_sheet)?
        .into_iter()
        .filter_map(|merge| {
            if !rects_intersect(merge, source) {
                None
            } else if rect_contains(source, merge) {
                Some(Ok(merge))
            } else {
                Some(Err(
                    "Cannot cut part of a merged cell; select the complete merged range"
                        .to_string(),
                ))
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
    for merge in merged_ranges(st, target_sheet)? {
        if rects_intersect(merge, target)
            && !(source_sheet == target_sheet && source_merges.contains(&merge))
            && !rect_contains(target, merge)
        {
            return Err(
                "Cut destination partially overlaps a merged cell; select its complete range"
                    .to_string(),
            );
        }
    }

    let row_delta = target.0 - source.0;
    let column_delta = target.1 - source.1;
    let mut rich_text = st.rich_text.clone();
    let moved_rich = st
        .rich_text
        .iter()
        .filter(|((sheet, row, column), _)| {
            *sheet == source_sheet
                && *row >= source.0
                && *row <= source.2
                && *column >= source.1
                && *column <= source.3
        })
        .map(|((_, row, column), runs)| ((*row, *column), runs.clone()))
        .collect::<Vec<_>>();
    rich_text.retain(|(sheet, row, column), _| {
        !(*sheet == source_sheet
            && *row >= source.0
            && *row <= source.2
            && *column >= source.1
            && *column <= source.3)
            && !(*sheet == target_sheet
                && *row >= target.0
                && *row <= target.2
                && *column >= target.1
                && *column <= target.3)
    });
    for ((row, column), runs) in moved_rich {
        rich_text.insert((target_sheet, row + row_delta, column + column_delta), runs);
    }
    let mut rich_text_xml = st.rich_text_xml.clone();
    let moved_rich_xml = st
        .rich_text_xml
        .iter()
        .filter(|((sheet, row, column), _)| {
            *sheet == source_sheet
                && *row >= source.0
                && *row <= source.2
                && *column >= source.1
                && *column <= source.3
        })
        .map(|((_, row, column), xml)| ((*row, *column), xml.clone()))
        .collect::<Vec<_>>();
    rich_text_xml.retain(|(sheet, row, column), _| {
        !(*sheet == source_sheet
            && *row >= source.0
            && *row <= source.2
            && *column >= source.1
            && *column <= source.3)
            && !(*sheet == target_sheet
                && *row >= target.0
                && *row <= target.2
                && *column >= target.1
                && *column <= target.3)
    });
    for ((row, column), xml) in moved_rich_xml {
        rich_text_xml.insert((target_sheet, row + row_delta, column + column_delta), xml);
    }

    let mut objects = st.objects.clone();
    let source_objects = objects.remove(&source_sheet).unwrap_or_default();
    let mut retained_objects = Vec::with_capacity(source_objects.len());
    let mut moved_objects = Vec::new();
    for mut object in source_objects {
        let object_row = object.get("r").and_then(Value::as_i64).unwrap_or(0) as i32;
        let object_column = object.get("c").and_then(Value::as_i64).unwrap_or(0) as i32;
        let native = object
            .get("config")
            .and_then(|config| config.get("nativeDrawing"));
        let anchor_kind = native
            .and_then(|descriptor| descriptor.get("anchorKind"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let native_kind = native
            .and_then(|descriptor| descriptor.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("drawing")
            .to_string();
        let cell_anchored = object.get("mode").and_then(Value::as_str) == Some("cell")
            || matches!(
                anchor_kind.as_deref(),
                Some("oneCellAnchor" | "twoCellAnchor")
            );
        let footprint = if anchor_kind.as_deref() == Some("twoCellAnchor") {
            let x = object.get("x").and_then(Value::as_f64).unwrap_or(0.0);
            let y = object.get("y").and_then(Value::as_f64).unwrap_or(0.0);
            let width = object.get("w").and_then(Value::as_f64).unwrap_or(0.0);
            let height = object.get("h").and_then(Value::as_f64).unwrap_or(0.0);
            let (end_column, _) =
                drawing_column_marker(&st.model, source_sheet, (x + width - 0.001).max(x));
            let (end_row, _) =
                drawing_row_marker(&st.model, source_sheet, (y + height - 0.001).max(y));
            (
                object_row,
                object_column,
                (end_row as i32 + 1).max(object_row),
                (end_column as i32 + 1).max(object_column),
            )
        } else {
            (object_row, object_column, object_row, object_column)
        };
        let intersects_source = rects_intersect(footprint, source);
        if cell_anchored && intersects_source && !rect_contains(source, footprint) {
            return Err(format!(
                "Cannot cut only part of cell-anchored object '{}'",
                object.get("id").and_then(Value::as_str).unwrap_or("object")
            ));
        }
        let in_source = cell_anchored && rect_contains(source, footprint);
        if !in_source || !cell_anchored {
            retained_objects.push(object);
            continue;
        }
        if native.is_some() {
            if source_sheet != target_sheet
                || !matches!(
                    native_kind.as_str(),
                    "chart" | "smartart" | "picture" | "shape" | "connector" | "group"
                )
                || st.source_ooxml.is_none()
            {
                return Err(format!(
                    "Cannot safely move native DrawingML object kind '{native_kind}' across worksheets; the cut was not applied"
                ));
            }
        }
        let moved_row = object_row + row_delta;
        let moved_column = object_column + column_delta;
        if moved_row < 1 || moved_column < 1 || moved_row > MAX_ROWS || moved_column > MAX_COLS {
            return Err("cell-anchored object would move outside the worksheet".to_string());
        }
        if object.get("x").is_some() || object.get("y").is_some() {
            let (source_x, source_y) =
                cell_origin_pixels(&st.model, source_sheet, object_row, object_column);
            let (target_x, target_y) =
                cell_origin_pixels(&st.model, target_sheet, moved_row, moved_column);
            let old_x = object.get("x").and_then(Value::as_f64).unwrap_or(source_x);
            let old_y = object.get("y").and_then(Value::as_f64).unwrap_or(source_y);
            object["x"] = json!(target_x + old_x - source_x);
            object["y"] = json!(target_y + old_y - source_y);
        }
        object["sheet"] = json!(target_sheet);
        object["r"] = json!(moved_row);
        object["c"] = json!(moved_column);
        moved_objects.push(object);
    }
    if source_sheet == target_sheet {
        retained_objects.extend(moved_objects);
        if !retained_objects.is_empty() {
            objects.insert(source_sheet, retained_objects);
        }
    } else {
        if !retained_objects.is_empty() {
            objects.insert(source_sheet, retained_objects);
        }
        objects
            .entry(target_sheet)
            .or_default()
            .extend(moved_objects);
    }

    let mut data_validations = st.worksheet_features.data_validations.clone();
    let source_transport = data_validations
        .get(&source_sheet)
        .cloned()
        .unwrap_or_default();
    let target_transport = data_validations
        .get(&target_sheet)
        .cloned()
        .unwrap_or_default();
    let mut moved_validations = Vec::new();
    let mut moved_source_indices = std::collections::HashSet::new();
    for (index, rule) in source_transport.rules.iter().enumerate() {
        let rectangles = parse_sqref_rectangles(&rule.sqref)?;
        if classify_rule_range(&rectangles, source)
            .map_err(|error| format!("Cannot safely cut data validation '{}': {error}", rule.id))?
        {
            let shifted = shift_rectangles(&rectangles, row_delta, column_delta)?;
            let source_anchor = (rectangles[0].0, rectangles[0].1);
            let target_anchor = (shifted[0].0, shifted[0].1);
            let source_area = area(source_sheet, source.0, source.1, source.2, source.3);
            let mut moved = rule.clone();
            moved.id = format!(
                "dv-move-{}",
                DATA_VALIDATION_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            moved.sqref = format_sqref_rectangles(&shifted);
            moved.formula1 = move_validation_formula(
                &mut st.model,
                moved.formula1,
                source_anchor,
                target_anchor,
                source_sheet,
                target_sheet,
                &source_area,
            )?;
            moved.formula2 = move_validation_formula(
                &mut st.model,
                moved.formula2,
                source_anchor,
                target_anchor,
                source_sheet,
                target_sheet,
                &source_area,
            )?;
            moved_validations.push(moved);
            moved_source_indices.insert(index);
        }
    }
    let target_removed_indices = target_transport
        .rules
        .iter()
        .enumerate()
        .filter_map(|(index, rule)| {
            if source_sheet == target_sheet && moved_source_indices.contains(&index) {
                return None;
            }
            let rectangles = parse_sqref_rectangles(&rule.sqref);
            Some(rectangles.and_then(|rectangles| {
                classify_rule_range(&rectangles, target)
                    .map_err(|error| {
                        format!(
                            "Cannot safely overwrite data validation '{}': {error}",
                            rule.id
                        )
                    })
                    .map(|selected| selected.then_some(index))
            }))
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect::<std::collections::HashSet<_>>();
    if source_sheet == target_sheet {
        let mut transport = source_transport;
        transport.rules = transport
            .rules
            .into_iter()
            .enumerate()
            .filter_map(|(index, rule)| {
                (!moved_source_indices.contains(&index) && !target_removed_indices.contains(&index))
                    .then_some(rule)
            })
            .collect();
        transport.rules.extend(moved_validations);
        data_validations.insert(source_sheet, transport);
    } else {
        let mut source_transport = source_transport;
        source_transport.rules = source_transport
            .rules
            .into_iter()
            .enumerate()
            .filter_map(|(index, rule)| (!moved_source_indices.contains(&index)).then_some(rule))
            .collect();
        data_validations.insert(source_sheet, source_transport);
        let mut target_transport = target_transport;
        target_transport.rules = target_transport
            .rules
            .into_iter()
            .enumerate()
            .filter_map(|(index, rule)| (!target_removed_indices.contains(&index)).then_some(rule))
            .collect();
        target_transport.rules.extend(moved_validations);
        data_validations.insert(target_sheet, target_transport);
    }
    let mut data_validation_dirty = st.worksheet_features.data_validation_dirty.clone();
    if !moved_source_indices.is_empty() || !target_removed_indices.is_empty() {
        data_validation_dirty.insert(source_sheet);
        data_validation_dirty.insert(target_sheet);
    }

    Ok(CutSidecarMovePlan {
        source_merges,
        rich_text,
        rich_text_xml,
        objects,
        data_validations,
        data_validation_dirty,
    })
}

fn apply_cut_sidecar_move(
    st: &mut AppState,
    source_sheet: u32,
    source: CellRect,
    target_sheet: u32,
    target: CellRect,
    plan: CutSidecarMovePlan,
) -> Result<(), String> {
    if !plan.source_merges.is_empty() {
        st.model
            .unmerge_cells_range(source_sheet, source.0, source.1, source.2, source.3)?;
        st.model
            .unmerge_cells_range(target_sheet, target.0, target.1, target.2, target.3)?;
        let row_delta = target.0 - source.0;
        let column_delta = target.1 - source.1;
        for (r0, c0, r1, c1) in plan.source_merges {
            st.model.merge_cells_range(
                target_sheet,
                r0 + row_delta,
                c0 + column_delta,
                r1 + row_delta,
                c1 + column_delta,
            )?;
        }
    }
    st.rich_text = plan.rich_text;
    st.rich_text_xml = plan.rich_text_xml;
    st.objects = plan.objects;
    st.worksheet_features.data_validations = plan.data_validations;
    st.worksheet_features.data_validation_dirty = plan.data_validation_dirty;
    Ok(())
}

// 粘贴：文本与最近一次应用内复制一致 → 走引擎剪贴板（复制=相对引用调整；
// 剪切=Excel 移动语义：公式不变、外部引用追随、清空源区）；否则按外部 TSV 逐格写入
fn api_paste(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let merged = merged_ranges(st, sheet)?;
    let (row, col) = merged_anchor_in(&merged, ji(&v, "row")? as i32, ji(&v, "col")? as i32);
    let text = v
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let is_cut = v.get("mode").and_then(|x| x.as_str()) == Some("cut");
    let special = v.get("special").and_then(Value::as_str).unwrap_or("all");

    let internal = st.clip_tsv.as_deref() == Some(text.as_str()) && st.clip_engine.is_some();
    if is_cut && !internal {
        return Err(
            "The internal cut source is no longer available; refusing to degrade the move to a text paste"
                .to_string(),
        );
    }
    if is_cut && special != "all" {
        return Err("Paste Special cannot consume an active cut range".to_string());
    }
    // A portable UniCell payload carries rich runs, merges, validation and
    // anchored objects.  Prefer it for copy; retain the engine path for cut so
    // formula move semantics and source clearing stay atomic.
    if internal && special == "all" && (is_cut || v.get("unicell").is_none()) {
        let clip_json = st.clip_engine.clone().ok_or("no clipboard")?;
        let src_sheet = clip_json["sheet"].as_u64().ok_or("bad clip sheet")? as u32;
        let rng = clip_json["range"]
            .as_array()
            .ok_or("bad clip range")?
            .iter()
            .map(|x| x.as_i64().unwrap_or(0) as i32)
            .collect::<Vec<i32>>();
        if rng.len() != 4 {
            return Err("bad clip range".into());
        }
        let (r0, c0, r1, c1) = (rng[0], rng[1], rng[2], rng[3]);
        let target_r1 = row + (r1 - r0);
        let target_c1 = col + (c1 - c0);
        if row < 1 || col < 1 || target_r1 > MAX_ROWS || target_c1 > MAX_COLS {
            return Err("cut destination is outside worksheet bounds".to_string());
        }
        let mut targets = std::collections::HashSet::new();
        for dr in 0..=(r1 - r0) {
            for dc in 0..=(c1 - c0) {
                let target = merged_anchor_in(&merged, row + dr, col + dc);
                if !targets.insert(target) {
                    return Err("cannot paste multiple cells into one merged cell".into());
                }
            }
        }
        let data: ClipboardData = serde_json::from_value(clip_json["data"].clone())
            .map_err(|e| format!("clip data: {e}"))?;
        let sidecar_plan = if is_cut {
            Some(prepare_cut_sidecar_move(
                st,
                src_sheet,
                (r0, c0, r1, c1),
                sheet,
                (row, col, target_r1, target_c1),
            )?)
        } else {
            None
        };
        st.model.set_selected_sheet(sheet)?;
        st.model.set_selected_cell(row, col)?;
        st.model.set_selected_range(row, col, row, col)?;
        st.model
            .paste_from_clipboard(src_sheet, (r0, c0, r1, c1), &data, is_cut)?;
        if let Some(plan) = sidecar_plan {
            apply_cut_sidecar_move(
                st,
                src_sheet,
                (r0, c0, r1, c1),
                sheet,
                (row, col, target_r1, target_c1),
                plan,
            )?;
            for affected_sheet in [src_sheet, sheet] {
                if st
                    .model
                    .get_conditional_formatting_list(affected_sheet)?
                    .is_empty()
                {
                    st.cf_sheets.remove(&affected_sheet);
                } else {
                    st.cf_sheets.insert(affected_sheet);
                }
            }
        }
        auto_expand_tables_for_area(st, sheet, row, col, target_r1, target_c1)?;
        if is_cut {
            // Consume the cut clipboard only after every fallible part of the move commits.
            st.clip_tsv = None;
            st.clip_engine = None;
        }
        return ok_json(json!({ "rows": r1 - r0 + 1, "cols": c1 - c0 + 1 }));
    }
    if !is_cut {
        if let Some(payload) = v.get("unicell") {
            let (rows, cols) = paste_unicell_payload(st, sheet, row, col, payload, special)?;
            if special != "formats" {
                auto_expand_tables_for_area(
                    st,
                    sheet,
                    row,
                    col,
                    row + rows as i32 - 1,
                    col + cols as i32 - 1,
                )?;
            }
            return ok_json(
                json!({ "rows": rows, "cols": cols, "rich": true, "special": special }),
            );
        }
    }
    // 外部文本：TSV 解析逐格写入
    let (rows, cols) = paste_external_text(st, sheet, row, col, &text)?;
    auto_expand_tables_for_area(
        st,
        sheet,
        row,
        col,
        row + rows as i32 - 1,
        col + cols as i32 - 1,
    )?;
    ok_json(json!({ "rows": rows, "cols": cols }))
}

fn calculation_settings_json(st: &AppState, changed: bool) -> Value {
    let iteration = st.model.get_iteration_settings();
    json!({
        "mode": st.calculation_mode.as_str(),
        "enabled": iteration.enabled,
        "maxIterations": iteration.maximum_iterations,
        "maxChange": iteration.maximum_change,
        "changed": changed,
    })
}

// Excel calculation properties are workbook-scoped.  `op:get` is read-only; `op:set`
// atomically updates calculation mode plus all iterative circular-reference limits.  A body
// containing only `mode` remains compatible with the original ribbon implementation.
fn api_calcmode(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let value = if body.is_empty() {
        json!({"op":"get"})
    } else {
        parse_body(body)?
    };
    let has_setting = ["mode", "enabled", "maxIterations", "maxChange"]
        .iter()
        .any(|key| value.get(*key).is_some());
    let operation = value
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or(if has_setting { "set" } else { "get" });
    if operation == "get" {
        return ok_json(calculation_settings_json(st, false));
    }
    if operation != "set" {
        return Err(format!("bad calcmode op: {operation}"));
    }

    let mode = match value.get("mode").and_then(Value::as_str) {
        None | Some("auto") => value
            .get("mode")
            .map(|_| CalculationMode::Automatic)
            .unwrap_or(st.calculation_mode),
        Some("manual") => CalculationMode::Manual,
        Some(other) => return Err(format!("unsupported calculation mode: {other}")),
    };
    let current = st.model.get_iteration_settings();
    let enabled = value
        .get("enabled")
        .map(|item| item.as_bool().ok_or("enabled must be a boolean"))
        .transpose()?
        .unwrap_or(current.enabled);
    let maximum_iterations = value
        .get("maxIterations")
        .map(|item| {
            item.as_u64()
                .and_then(|number| u32::try_from(number).ok())
                .ok_or("maxIterations must be an unsigned 32-bit integer")
        })
        .transpose()?
        .unwrap_or(current.maximum_iterations);
    let maximum_change = value
        .get("maxChange")
        .map(|item| item.as_f64().ok_or("maxChange must be a finite number"))
        .transpose()?
        .unwrap_or(current.maximum_change);
    let next = IterationSettings {
        enabled,
        maximum_iterations,
        maximum_change,
    };
    let changed = mode != st.calculation_mode || next != current;
    if changed {
        // Validation happens before any mode/dirty-state mutation, so a rejected request is
        // transactionally invisible to the application history.
        st.model.set_iteration_settings(next)?;
        st.calculation_mode = mode;
        st.calculation_properties_dirty = true;
        match mode {
            CalculationMode::Automatic => {
                st.model.resume_evaluation();
                st.model.evaluate();
            }
            CalculationMode::Manual => st.model.pause_evaluation(),
        }
    }
    ok_json(calculation_settings_json(st, changed))
}

// 名称管理器（defined names）：list / add / del
fn api_names(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    match js(&v, "op")? {
        "list" => {
            let list = st.model.get_defined_name_list();
            let arr: Vec<Value> = list
                .iter()
                .map(|(name, scope, formula)| {
                    json!({ "name": name, "scope": scope, "formula": formula })
                })
                .collect();
            ok_json(json!({ "names": arr }))
        }
        "add" => {
            let name = js(&v, "name")?;
            let formula = js(&v, "formula")?;
            if st.model.is_valid_defined_name(name, None, formula).is_err() {
                return Err(format!("无效的名称: {name}"));
            }
            st.model.new_defined_name(name, None, formula)?;
            ok_json(json!({}))
        }
        "del" => {
            st.model.delete_defined_name(js(&v, "name")?, None)?;
            ok_json(json!({}))
        }
        other => Err(format!("bad names op: {other}")),
    }
}

// 追踪从属单元格：扫描本表已用区域公式，反查谁引用了目标格
fn api_dependents(st: &AppState, query: &str) -> Result<Resp, String> {
    let sheet = qi(query, "sheet", 0) as u32;
    let row = qi(query, "row", 1).clamp(1, MAX_ROWS);
    let col = qi(query, "col", 1).clamp(1, MAX_COLS);
    let target = cell_ref_str(row, col);
    let ws = st
        .model
        .get_model()
        .workbook
        .worksheet(sheet)
        .map_err(|e| e.to_string())?;
    let d = ws.dimension();
    let mut out = Vec::new();
    for r in d.min_row..=d.max_row {
        for c in d.min_column..=d.max_column {
            if r == row && c == col {
                continue;
            }
            let content = st.model.get_cell_content(sheet, r, c)?;
            if !content.starts_with('=') {
                continue;
            }
            if formula_references_cell(&content, &target) {
                out.push(json!({ "r": r, "c": c }));
                if out.len() >= 200 {
                    return ok_json(json!({ "dependents": out }));
                }
            }
        }
    }
    ok_json(json!({ "dependents": out }))
}

fn cell_ref_str(r: i32, c: i32) -> String {
    format!("{}{}", num_to_col(c as i64), r)
}

// 判断公式是否引用了指定单元格（跳过字符串，支持区域引用包含）
fn formula_references_cell(formula: &str, target: &str) -> bool {
    let upper = formula.to_uppercase();
    let tgt = target.to_uppercase();
    let chars: Vec<char> = upper.chars().collect();
    let mut in_str = false;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '"' {
            in_str = !in_str;
            i += 1;
            continue;
        }
        if in_str {
            i += 1;
            continue;
        }
        // 匹配单格引用 [$]COL[$]ROW，前后非字母数字
        if ch.is_ascii_uppercase() || ch == '$' {
            let start = i;
            let mut j = i;
            if chars[j] == '$' {
                j += 1;
            }
            let cs = j;
            while j < chars.len() && chars[j].is_ascii_uppercase() {
                j += 1;
            }
            if chars.get(j) == Some(&'$') {
                j += 1;
            }
            let rs = j;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let prev_ok = start == 0
                || !(chars[start - 1].is_ascii_alphanumeric() || chars[start - 1] == '_');
            let next_ok =
                j >= chars.len() || !(chars[j].is_ascii_alphanumeric() || chars[j] == '_');
            if j > rs && j > cs && prev_ok && next_ok {
                let tok: String = chars[start..j].iter().collect();
                let clean = tok.replace('$', "");
                if clean == tgt {
                    return true;
                }
                // 区域引用包含检查（如 A1:A10 包含 A5）
                if tgt
                    .chars()
                    .next()
                    .map(|x| x.is_ascii_uppercase())
                    .unwrap_or(false)
                {
                    // 若当前 token 是区域的一端，比较数字范围
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

// 朴素的公式相对引用平移（供 Ctrl+Enter 区域填充）：跳过字符串字面量与 $ 绝对引用
fn shift_formula_refs(formula: &str, dr: i32, dc: i32) -> String {
    let chars: Vec<char> = formula.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let n = chars.len();
    let mut in_str = false;
    while i < n {
        let ch = chars[i];
        if in_str {
            out.push(ch);
            if ch == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_str = true;
            out.push(ch);
            i += 1;
            continue;
        }
        // 尝试匹配 [$]COL[$]ROW
        let start = i;
        let mut j = i;
        let col_abs = j < n && chars[j] == '$';
        if col_abs {
            j += 1;
        }
        let col_start = j;
        while j < n && chars[j].is_ascii_uppercase() {
            j += 1;
        }
        let col_len = j - col_start;
        let row_abs = j < n && chars[j] == '$';
        if row_abs {
            j += 1;
        }
        let row_start = j;
        while j < n && chars[j].is_ascii_digit() {
            j += 1;
        }
        let row_len = j - row_start;
        // 前一个字符不能是字母/数字/下划线（避免函数名/命名区域误判）
        let prev_ok =
            start == 0 || !(chars[start - 1].is_ascii_alphanumeric() || chars[start - 1] == '_');
        // 后一个字符不能是字母/数字（避免 A11 匹配成 A1+1）
        let next_ok = j >= n || !(chars[j].is_ascii_alphanumeric() || chars[j] == '_');
        if col_len >= 1 && col_len <= 3 && row_len >= 1 && row_len <= 7 && prev_ok && next_ok {
            let col_txt: String = chars[col_start..col_start + col_len].iter().collect();
            let row_txt: String = chars[row_start..row_start + row_len].iter().collect();
            let mut col_num: i64 = 0;
            for c in col_txt.chars() {
                col_num = col_num * 26 + (c as i64 - 'A' as i64 + 1);
            }
            let mut row_num: i64 = row_txt.parse().unwrap_or(0);
            if !col_abs {
                col_num += dc as i64;
            }
            if !row_abs {
                row_num += dr as i64;
            }
            if col_num < 1 || row_num < 1 || col_num > MAX_COLS as i64 || row_num > MAX_ROWS as i64
            {
                out.push_str("#REF!");
            } else {
                if col_abs {
                    out.push('$');
                }
                out.push_str(&num_to_col(col_num));
                if row_abs {
                    out.push('$');
                }
                out.push_str(&row_num.to_string());
            }
            i = j;
        } else {
            out.push(ch);
            i += 1;
        }
    }
    out
}

fn num_to_col(mut n: i64) -> String {
    let mut s = String::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        s.insert(0, (b'A' + rem) as char);
        n = (n - 1) / 26;
    }
    s
}

// Ctrl+方向键：跳到数据块边缘（Excel 语义）
fn api_edge(st: &AppState, query: &str) -> Result<Resp, String> {
    let sheet = qi(query, "sheet", 0) as u32;
    let row = qi(query, "row", 1).clamp(1, MAX_ROWS);
    let col = qi(query, "col", 1).clamp(1, MAX_COLS);
    let dir = qget(query, "dir").unwrap_or("down");
    let (dr, dc) = match dir {
        "up" => (-1, 0),
        "down" => (1, 0),
        "left" => (0, -1),
        _ => (0, 1),
    };
    let non_empty = |r: i32, c: i32| -> bool {
        if r < 1 || c < 1 || r > MAX_ROWS || c > MAX_COLS {
            return false;
        }
        !st.model
            .get_formatted_cell_value(sheet, r, c)
            .unwrap_or_default()
            .is_empty()
    };
    let (mut r, mut c) = (row, col);
    let cur = non_empty(r, c);
    let next = non_empty(r + dr, c + dc);
    let limit = 200_000; // 扫描步数上限
    let mut steps = 0;
    if cur && next {
        // 走到数据块末尾
        while non_empty(r + dr, c + dc) && steps < limit {
            r += dr;
            c += dc;
            steps += 1;
        }
    } else {
        // 跳过空档，落到下一个非空；没有则到边界
        r += dr;
        c += dc;
        while !non_empty(r, c)
            && r >= 1
            && c >= 1
            && r <= MAX_ROWS
            && c <= MAX_COLS
            && steps < limit
        {
            r += dr;
            c += dc;
            steps += 1;
        }
        if r < 1 || c < 1 || r > MAX_ROWS || c > MAX_COLS {
            r = r.clamp(1, MAX_ROWS);
            c = c.clamp(1, MAX_COLS);
        }
    }
    ok_json(json!({ "row": r, "col": c }))
}

// 状态栏统计：选区内数字求和/计数/平均
fn api_stats(st: &AppState, query: &str) -> Result<Resp, String> {
    let sheet = qi(query, "sheet", 0) as u32;
    let (r0, c0, r1, c1) = clamp_range(
        qi(query, "r0", 1),
        qi(query, "c0", 1),
        qi(query, "r1", 1),
        qi(query, "c1", 1),
    );
    if (r1 - r0 + 1) as i64 * (c1 - c0 + 1) as i64 > 200_000 {
        return ok_json(json!({ "count": 0, "numbers": 0, "sum": 0.0, "avg": 0.0 }));
    }
    let mut count = 0u64;
    let mut numbers = 0u64;
    let mut sum = 0.0f64;
    for r in r0..=r1 {
        for c in c0..=c1 {
            let v = st.model.get_formatted_cell_value(sheet, r, c)?;
            if v.is_empty() {
                continue;
            }
            count += 1;
            let t = format!("{:?}", st.model.get_cell_type(sheet, r, c)?);
            if t == "Number" {
                // 从原始内容/计算值取数字：格式化文本可能带货币符
                let raw = st.model.get_cell_content(sheet, r, c)?;
                let parsed = raw.parse::<f64>().ok().or_else(|| {
                    let cleaned: String = v
                        .chars()
                        .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-')
                        .collect();
                    cleaned.parse::<f64>().ok()
                });
                if let Some(x) = parsed {
                    numbers += 1;
                    sum += x;
                }
            }
        }
    }
    let avg = if numbers > 0 {
        sum / numbers as f64
    } else {
        0.0
    };
    ok_json(json!({ "count": count, "numbers": numbers, "sum": sum, "avg": avg }))
}

fn api_freeze(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    st.model
        .set_frozen_rows_count(sheet, ji(&v, "rows")? as i32)?;
    st.model
        .set_frozen_columns_count(sheet, ji(&v, "cols")? as i32)?;
    ok_json(json!({}))
}

// 已用区域（Ctrl+A 选数据区/查找范围）
fn api_dimension(st: &AppState, query: &str) -> Result<Resp, String> {
    let sheet = qi(query, "sheet", 0) as u32;
    let ws = st
        .model
        .get_model()
        .workbook
        .worksheet(sheet)
        .map_err(|e| e.to_string())?;
    let d = ws.dimension();
    ok_json(json!({
        "minRow": d.min_row, "maxRow": d.max_row,
        "minCol": d.min_column, "maxCol": d.max_column,
    }))
}

// 查找：在已用区域扫描，返回匹配单元格列表（Excel 语义：默认匹配显示值子串，可选区分大小写/整单元格/查公式）
fn api_find(st: &AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let text = js(&v, "text")?;
    if text.is_empty() {
        return ok_json(json!({ "matches": [] }));
    }
    let match_case = v
        .get("matchCase")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let whole_cell = v
        .get("wholeCell")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let in_formulas = v
        .get("inFormulas")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let ws = st
        .model
        .get_model()
        .workbook
        .worksheet(sheet)
        .map_err(|e| e.to_string())?;
    let d = ws.dimension();
    let needle = if match_case {
        text.to_string()
    } else {
        text.to_lowercase()
    };
    let mut matches = Vec::new();
    'outer: for r in d.min_row..=d.max_row {
        for c in d.min_column..=d.max_column {
            let hay_raw = if in_formulas {
                st.model.get_cell_content(sheet, r, c)?
            } else {
                st.model.get_formatted_cell_value(sheet, r, c)?
            };
            if hay_raw.is_empty() {
                continue;
            }
            let hay = if match_case {
                hay_raw.clone()
            } else {
                hay_raw.to_lowercase()
            };
            let hit = if whole_cell {
                hay == needle
            } else {
                hay.contains(&needle)
            };
            if hit {
                matches.push(json!({ "r": r, "c": c, "v": hay_raw }));
                if matches.len() >= 1000 {
                    break 'outer;
                }
            }
        }
    }
    ok_json(json!({ "matches": matches }))
}

// 替换：对单个单元格或全部匹配执行。Excel 语义：替换作用于单元格原始内容（公式文本）
fn api_replace(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let sheet = ji(&v, "sheet")? as u32;
    let find = js(&v, "find")?.to_string();
    let replace = js(&v, "replace")?.to_string();
    if find.is_empty() {
        return Err("empty find text".into());
    }
    let match_case = v
        .get("matchCase")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let all = v.get("all").and_then(|x| x.as_bool()).unwrap_or(false);
    let mut targets: Vec<(i32, i32)> = Vec::new();
    if all {
        let ws = st
            .model
            .get_model()
            .workbook
            .worksheet(sheet)
            .map_err(|e| e.to_string())?;
        let d = ws.dimension();
        for r in d.min_row..=d.max_row {
            for c in d.min_column..=d.max_column {
                targets.push((r, c));
            }
        }
    } else {
        targets.push((ji(&v, "row")? as i32, ji(&v, "col")? as i32));
    }
    let mut replaced = 0u32;
    st.model.pause_evaluation();
    for (r, c) in targets {
        let content = st.model.get_cell_content(sheet, r, c)?;
        if content.is_empty() {
            continue;
        }
        let new_content = if match_case {
            if !content.contains(&find) {
                continue;
            }
            content.replace(&find, &replace)
        } else {
            let lower = content.to_lowercase();
            let flower = find.to_lowercase();
            if !lower.contains(&flower) {
                continue;
            }
            // 大小写不敏感替换：逐段重建
            let mut out = String::new();
            let mut rest = content.as_str();
            loop {
                let rl = rest.to_lowercase();
                match rl.find(&flower) {
                    Some(pos) => {
                        // 按字符边界安全切分（pos 来自 lowercase，长度可能不同，需映射）
                        let mut byte_pos = pos;
                        if rl.len() != rest.len() {
                            // 回退：逐字符对齐
                            byte_pos = 0;
                            let mut found = false;
                            for (i, _) in rest.char_indices() {
                                let cand = &rest[i..];
                                if cand.to_lowercase().starts_with(&flower) {
                                    byte_pos = i;
                                    found = true;
                                    break;
                                }
                            }
                            if !found {
                                out.push_str(rest);
                                rest = "";
                                break;
                            }
                        }
                        out.push_str(&rest[..byte_pos]);
                        out.push_str(&replace);
                        // 跳过原文中匹配长度（按字符数对齐）
                        let match_chars = flower.chars().count();
                        let mut skipped = 0;
                        let mut next_idx = rest.len();
                        for (i, _) in rest[byte_pos..].char_indices() {
                            if skipped == match_chars {
                                next_idx = byte_pos + i;
                                break;
                            }
                            skipped += 1;
                        }
                        if skipped < match_chars {
                            next_idx = rest.len();
                        }
                        rest = &rest[next_idx..];
                        if rest.is_empty() {
                            break;
                        }
                    }
                    None => {
                        out.push_str(rest);
                        break;
                    }
                }
            }
            out
        };
        if new_content != content {
            st.model.set_user_input(sheet, r, c, &new_content)?;
            replaced += 1;
        }
    }
    st.model.resume_evaluation();
    st.model.evaluate();
    ok_json(json!({ "replaced": replaced }))
}

fn api_import(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    if body.is_empty() {
        return Err("empty upload".into());
    }
    let names = load_xlsx_into_state(st, body)?;
    ok_json(json!({ "sheets": names }))
}

fn decode_csv_text(body: &[u8]) -> Result<String, String> {
    if body.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return std::str::from_utf8(&body[3..])
            .map(str::to_owned)
            .map_err(|_| "CSV 的 UTF-8 BOM 后包含无效文本".to_string());
    }
    if body.starts_with(&[0xFF, 0xFE]) || body.starts_with(&[0xFE, 0xFF]) {
        let little_endian = body.starts_with(&[0xFF, 0xFE]);
        let bytes = &body[2..];
        if bytes.len() % 2 != 0 {
            return Err("CSV 的 UTF-16 字节长度无效".into());
        }
        let units = bytes
            .chunks_exact(2)
            .map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect::<Vec<_>>();
        return String::from_utf16(&units).map_err(|_| "CSV 包含无效的 UTF-16 文本".into());
    }
    if let Ok(text) = std::str::from_utf8(body) {
        return Ok(text.to_owned());
    }
    // Windows 简体中文 Excel 的传统“CSV（逗号分隔）”通常使用系统 GBK 编码。
    let (decoded, _, had_errors) = encoding_rs::GBK.decode(body);
    if had_errors {
        return Err("CSV 不是有效的 UTF-8、UTF-16 或 GBK 文本".into());
    }
    Ok(decoded.into_owned())
}

fn csv_numeric_text_requires_exact_storage(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.starts_with('=') {
        return false;
    }
    let unsigned = trimmed
        .strip_prefix('+')
        .or_else(|| trimmed.strip_prefix('-'))
        .unwrap_or(trimmed);
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map(|(left, right)| (left, Some(right)))
        .unwrap_or((unsigned, None));
    if exponent.is_some_and(|value| {
        let digits = value
            .strip_prefix('+')
            .or_else(|| value.strip_prefix('-'))
            .unwrap_or(value);
        digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit())
    }) {
        return false;
    }
    if mantissa.is_empty()
        || mantissa
            .chars()
            .filter(|character| *character == '.')
            .count()
            > 1
        || !mantissa
            .chars()
            .all(|character| character == '.' || character.is_ascii_digit())
    {
        return false;
    }
    let integer = mantissa.split('.').next().unwrap_or("");
    let leading_zero_identifier = integer.len() > 1 && integer.starts_with('0');
    let significant_digits = mantissa
        .chars()
        .filter(|character| character.is_ascii_digit())
        .skip_while(|character| *character == '0')
        .count();
    leading_zero_identifier || significant_digits > 15
}

fn api_import_csv(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    if body.is_empty() {
        return Err("CSV 文件为空".into());
    }
    let text = decode_csv_text(body)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    let mut max_columns = 0usize;
    for record in reader.records() {
        let record = record.map_err(|error| format!("CSV 解析失败：{error}"))?;
        if rows.len() >= MAX_ROWS as usize {
            return Err(format!("CSV 超过最大行数 {MAX_ROWS}"));
        }
        if record.len() > MAX_COLS as usize {
            return Err(format!("CSV 超过最大列数 {MAX_COLS}"));
        }
        max_columns = max_columns.max(record.len());
        rows.push(record.iter().map(str::to_owned).collect::<Vec<_>>());
    }

    let storage_scope = st.storage_scope.clone();
    let mut imported = AppState::new_with_storage_scope(storage_scope);
    imported.model.rename_sheet(0, "工作表1")?;
    imported.model.pause_evaluation();
    for (row_index, row) in rows.iter().enumerate() {
        for (column_index, value) in row.iter().enumerate() {
            if !value.is_empty() {
                let row = row_index as i32 + 1;
                let column = column_index as i32 + 1;
                if csv_numeric_text_requires_exact_storage(value) {
                    imported
                        .model
                        .set_rich_text_plain_value(0, row, column, value)?;
                } else {
                    imported.model.set_user_input(0, row, column, value)?;
                }
            }
        }
    }
    imported.model.resume_evaluation();
    imported.model.evaluate();
    imported.model.set_selected_sheet(0)?;
    imported.model.set_selected_cell(1, 1)?;
    imported.model.discard_all_history();
    let _ = imported.model.flush_send_queue();
    imported.file_name = "CSV".to_string();
    imported.clear_application_history();
    let names = imported.model.get_model().workbook.get_worksheet_names();
    *st = imported;
    ok_json(json!({
        "sheets": names,
        "rows": rows.len(),
        "columns": max_columns,
    }))
}

fn api_export_csv(st: &AppState, query: &str) -> Result<Resp, String> {
    let sheet_index = qi(query, "sheet", 0);
    if sheet_index < 0 {
        return Err("CSV 工作表索引无效".into());
    }
    let sheet = sheet_index as u32;
    let sheet_names = st.model.get_model().workbook.get_worksheet_names();
    if sheet as usize >= sheet_names.len() {
        return Err("CSV 工作表不存在".into());
    }
    let worksheet = st
        .model
        .get_model()
        .workbook
        .worksheet(sheet)
        .map_err(|error| error.to_string())?;
    let dimension = worksheet.dimension();

    // UTF-8 BOM lets desktop Excel recognize Chinese without an import wizard.
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    {
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .terminator(csv::Terminator::CRLF)
            .from_writer(&mut bytes);
        for row in 1..=dimension.max_row.max(1) {
            let mut record = Vec::with_capacity(dimension.max_column.max(1) as usize);
            for column in 1..=dimension.max_column.max(1) {
                // CSV is a presentation interchange format: formulas become their visible values,
                // matching spreadsheet “save active sheet as CSV” behavior. Literal values use
                // their content representation instead of the viewport formatter: General may
                // display a large integer as a rounded scientific value, which would otherwise
                // irreversibly corrupt a CSV roundtrip.
                let content = st.model.get_cell_content(sheet, row, column)?;
                let value = if content.starts_with('=') || content.starts_with('\'') {
                    st.model.get_formatted_cell_value(sheet, row, column)?
                } else {
                    content
                };
                record.push(value);
            }
            writer
                .write_record(&record)
                .map_err(|error| format!("CSV 写入失败：{error}"))?;
        }
        writer
            .flush()
            .map_err(|error| format!("CSV 写入失败：{error}"))?;
    }

    let basename = export_basename(st, query);
    let filename = http_safe_attachment_name(&basename, "csv");
    Ok(tiny_http::Response::from_data(bytes)
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/csv; charset=utf-8"[..])
                .unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Disposition"[..], filename.as_bytes())
                .unwrap(),
        ))
}

fn normalize_opc_part_name(name: &str) -> Option<String> {
    let mut clean = Vec::new();
    let normalized = name.replace('\\', "/");
    for segment in normalized.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            value => clean.push(value),
        }
    }
    if clean.is_empty() {
        None
    } else {
        Some(clean.join("/"))
    }
}

fn snapshot_opc_package(body: &[u8]) -> Result<OpcPackageSnapshot, String> {
    use std::io::{Cursor, Read};
    let mut archive =
        zip::read::ZipArchive::new(Cursor::new(body)).map_err(|e| format!("OPC package: {e}"))?;
    let mut parts = std::collections::BTreeMap::new();
    let mut total = 0usize;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| format!("OPC entry: {e}"))?;
        if file.is_dir() {
            continue;
        }
        let name = normalize_opc_part_name(file.name()).ok_or("unsafe OPC part name")?;
        let declared = usize::try_from(file.size()).map_err(|_| "OPC part is too large")?;
        total = total
            .checked_add(declared)
            .ok_or("OPC package size overflow")?;
        if total > MAX_OPC_UNCOMPRESSED {
            return Err(format!(
                "OPC package expands beyond {} MiB",
                MAX_OPC_UNCOMPRESSED / 1024 / 1024
            ));
        }
        let mut bytes = Vec::with_capacity(declared.min(8 * 1024 * 1024));
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("OPC read {name}: {e}"))?;
        parts.insert(name, bytes);
    }
    let content_types = parts
        .get("[Content_Types].xml")
        .map(|v| String::from_utf8_lossy(v))
        .unwrap_or_default();
    let macro_enabled = parts.contains_key("xl/vbaProject.bin")
        || content_types.contains("application/vnd.ms-excel.sheet.macroEnabled.main+xml")
        || content_types.contains("application/vnd.ms-excel.template.macroEnabled.main+xml");
    Ok(OpcPackageSnapshot {
        parts,
        macro_enabled,
    })
}

fn encode_opc_parts(parts: std::collections::BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Write};
    let mut writer = zip::write::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in parts {
        writer
            .start_file(name, options)
            .map_err(|error| format!("OPC write: {error}"))?;
        writer
            .write_all(&bytes)
            .map_err(|error| format!("OPC write data: {error}"))?;
    }
    Ok(writer
        .finish()
        .map_err(|error| format!("OPC finish: {error}"))?
        .into_inner())
}

fn parse_ooxml_boolean(value: Option<&str>, fallback: bool) -> bool {
    match value {
        Some("1" | "true" | "TRUE") => true,
        Some("0" | "false" | "FALSE") => false,
        _ => fallback,
    }
}

fn imported_calculation_properties(
    snapshot: &OpcPackageSnapshot,
) -> (CalculationMode, IterationSettings) {
    let defaults = IterationSettings::default();
    let Some(xml) = snapshot
        .parts
        .get("xl/workbook.xml")
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return (CalculationMode::Automatic, defaults);
    };
    let Ok(document) = roxmltree::Document::parse(xml) else {
        return (CalculationMode::Automatic, defaults);
    };
    let Some(calc_pr) = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "calcPr")
    else {
        return (CalculationMode::Automatic, defaults);
    };
    let mode = if calc_pr.attribute("calcMode") == Some("manual") {
        CalculationMode::Manual
    } else {
        // Excel's third token, autoNoTable, is automatic from UniCell's two-mode UI and is
        // preserved byte-for-byte until the user explicitly changes calculation settings.
        CalculationMode::Automatic
    };
    let maximum_iterations = calc_pr
        .attribute("iterateCount")
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (1..=32_767).contains(value))
        .unwrap_or(defaults.maximum_iterations);
    let maximum_change = calc_pr
        .attribute("iterateDelta")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(defaults.maximum_change);
    (
        mode,
        IterationSettings {
            enabled: parse_ooxml_boolean(calc_pr.attribute("iterate"), defaults.enabled),
            maximum_iterations,
            maximum_change,
        },
    )
}

// 把 xlsx 字节载入当前状态（共享给 import / import-html / import-udoc），返回 sheet 名列表
fn load_xlsx_into_state(st: &mut AppState, body: &[u8]) -> Result<Vec<String>, String> {
    let t0 = std::time::Instant::now();
    // Capture the package before IronCalc flattens it. Unsupported Excel features are restored
    // byte-for-byte after the controlled workbook parts have been regenerated.
    let source_ooxml = snapshot_opc_package(body)?;
    let tmp = unique_xlsx_temp_path("import");
    std::fs::write(&tmp, body).map_err(|e| format!("write temp: {e}"))?;
    let parsed = load_from_xlsx(tmp.to_str().ok_or("bad temp path")?, "en", "UTC", "en")
        .map_err(|e| format!("xlsx parse: {e}"));
    let _ = std::fs::remove_file(&tmp);
    let model = parsed?;
    let t_parse = t0.elapsed();
    let t1 = std::time::Instant::now();
    st.model = UserModel::from_model(model);
    let (calculation_mode, iteration_settings) = imported_calculation_properties(&source_ooxml);
    st.model.set_iteration_settings(iteration_settings)?;
    st.calculation_mode = calculation_mode;
    st.calculation_properties_dirty = false;
    match calculation_mode {
        CalculationMode::Automatic => {
            st.model.resume_evaluation();
            st.model.evaluate();
        }
        CalculationMode::Manual => st.model.pause_evaluation(),
    }
    // 精确解析 workbook 关系实际指向的主题部件（IronCalc 导入器可能不解析主题）。
    // 主题文件名不保证是 theme1.xml，例如 Excel 会合法保留 theme/theme9.xml。
    if let Some(theme) = parse_xlsx_theme(&source_ooxml) {
        st.model.set_theme(theme);
    }
    let theme = st.model.get_model().workbook.theme.clone();
    let (rich_text, rich_text_xml) = parse_xlsx_rich_text(body, &theme);
    st.rich_text = rich_text;
    st.rich_text_xml = rich_text_xml;
    st.formula_transport = parse_formula_transport(&source_ooxml, &st.model);
    st.excel_extension = if source_ooxml.macro_enabled {
        "xlsm"
    } else {
        "xlsx"
    }
    .to_string();
    st.excel_mime = if source_ooxml.macro_enabled {
        "application/vnd.ms-excel.sheet.macroEnabled.12"
    } else {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    }
    .to_string();
    st.source_ooxml = Some(source_ooxml);
    // 重建 CF 标记：扫描每个 sheet 是否带条件格式
    st.cf_sheets.clear();
    st.worksheet_features
        .baseline_conditional_formatting
        .clear();
    st.worksheet_features.data_validations.clear();
    st.worksheet_features.data_validation_dirty.clear();
    st.pivot_cache_refresh_edits.clear();
    st.native_pivot_table_edits.clear();
    st.native_pivot_local_refresh_edits.clear();
    st.native_slicer_edits.clear();
    st.native_timeline_edits.clear();
    st.native_data_edits.clear();
    st.native_table_edits.clear();
    st.native_page_review_edits.clear();
    st.native_table_model = st
        .source_ooxml
        .as_ref()
        .map(|snapshot| native_table_edit::parse_native_table_model(&snapshot.parts))
        .transpose()?;
    st.what_if_scenarios = st
        .source_ooxml
        .as_ref()
        .map(|snapshot| {
            let sheet_parts = snapshot_workbook_sheet_paths(snapshot);
            what_if_runtime::import_scenarios(&snapshot.parts, &sheet_parts)
        })
        .unwrap_or_default();
    if let Some(table_model) = st.native_table_model.as_ref() {
        st.model
            .replace_tables(ironcalc_tables_from_native(table_model)?);
    }
    if let Some(snapshot) = st.source_ooxml.as_ref() {
        for (sheet, path) in snapshot_workbook_sheet_paths(snapshot)
            .into_iter()
            .enumerate()
        {
            let Some(xml) = snapshot
                .parts
                .get(&path)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            else {
                continue;
            };
            let parsed = parse_data_validation_sheet(sheet as u32, xml);
            if !parsed.rules.is_empty() || !parsed.container_start_tag.is_empty() {
                st.worksheet_features
                    .data_validations
                    .insert(sheet as u32, parsed);
            }
        }
    }
    // 提取内嵌 drawing 图片为 UniCell 对象（绝对定位，精确保留 Excel 中的像素位置）
    st.objects = import_drawings_objects(body, &st.model);
    let sheet_count = st.model.get_model().workbook.worksheets.len() as u32;
    for si in 0..sheet_count {
        if let Ok(list) = st.model.get_conditional_formatting_list(si) {
            if let Ok(serialized) = serde_json::to_string(&list) {
                st.worksheet_features
                    .baseline_conditional_formatting
                    .insert(si, serialized);
            }
            if !list.is_empty() {
                st.cf_sheets.insert(si);
            }
        }
    }
    let t_from = t1.elapsed();
    eprintln!(
        "[import] bytes={} parse={}ms from_model={}ms",
        body.len(),
        t_parse.as_millis(),
        t_from.as_millis()
    );
    st.clip_tsv = None;
    st.clip_engine = None;
    Ok(st.model.get_model().workbook.get_worksheet_names())
}

fn udoc_sheet_chunk(st: &AppState, sheet: u32, name: &str) -> Result<Value, String> {
    // Iterate the engine's actual row-first cell store, not the rectangular dimension. A sparse
    // workbook may contain A1 and XFD1048576 only; scanning that bounding box would be both slow
    // and impossible to serialize compactly.
    let mut coordinates = st
        .model
        .get_model()
        .workbook
        .worksheet(sheet)
        .map_err(|error| error.to_string())?
        .sheet_data
        .iter()
        .flat_map(|(row, columns)| columns.keys().map(move |col| (*row, *col)))
        .collect::<Vec<_>>();
    coordinates.sort_unstable();
    let mut populated = std::collections::BTreeMap::new();
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    for (row, col) in coordinates {
        let content = st.model.get_cell_content(sheet, row, col)?;
        if content.is_empty() {
            continue;
        }
        let formatted = st.model.get_formatted_cell_value(sheet, row, col)?;
        populated.insert((row, col), (content, formatted));
        bounds = Some(match bounds {
            None => (row, col, row, col),
            Some((r0, c0, r1, c1)) => (r0.min(row), c0.min(col), r1.max(row), c1.max(col)),
        });
    }
    let Some((r0, c0, r1, c1)) = bounds else {
        return Ok(json!({
            "schema": "unicell-row-major-v2",
            "layouts": ["dense", "sparse"],
            "layout": "dense",
            "derived": true,
            "authoritative": false,
            "source": "document/workbook.xlsx",
            "sheet": name,
            "range": Value::Null,
            "origin": "A1",
            "rows": [],
        }));
    };
    let rectangle_cells = (r1 - r0 + 1) as i64 * (c1 - c0 + 1) as i64;
    let populated_cells = populated.len() as i64;
    if populated_cells > ai_context::MAX_DIFF_CELLS * 20 {
        return Err(format!(
            "udoc 派生视图有 {populated_cells} 个非空单元格，超过安全上限；请用 derived=0 导出仅权威工作簿",
        ));
    }
    let dense = rectangle_cells <= ai_context::MAX_DIFF_CELLS * 20
        && rectangle_cells <= populated_cells.saturating_mul(4);
    let mut display = serde_json::Map::new();
    let rows = if dense {
        let mut rows = Vec::with_capacity((r1 - r0 + 1) as usize);
        for row in r0..=r1 {
            let mut line = Vec::with_capacity((c1 - c0 + 1) as usize);
            for col in c0..=c1 {
                match populated.get(&(row, col)) {
                    Some((content, formatted)) => {
                        line.push(json!(content));
                        if formatted != content {
                            display.insert(ai_context::format_cell(row, col), json!(formatted));
                        }
                    }
                    None => line.push(Value::Null),
                }
            }
            rows.push(Value::Array(line));
        }
        rows
    } else {
        // Sparse row-major encoding: [rowOffset, [[columnOffset, content], ...]]. It remains easy
        // for an AI/tool reader to stream by row while never materializing null-filled gaps.
        let mut rows = Vec::new();
        let mut current_row = None;
        let mut current_cells = Vec::new();
        for ((row, col), (content, formatted)) in &populated {
            if current_row != Some(*row) {
                if let Some(previous) = current_row {
                    rows.push(json!([previous - r0, current_cells]));
                    current_cells = Vec::new();
                }
                current_row = Some(*row);
            }
            current_cells.push(json!([col - c0, content]));
            if formatted != content {
                display.insert(ai_context::format_cell(*row, *col), json!(formatted));
            }
        }
        if let Some(previous) = current_row {
            rows.push(json!([previous - r0, current_cells]));
        }
        rows
    };
    let mut chunk = json!({
        "schema": "unicell-row-major-v2",
        "layout": if dense { "dense" } else { "sparse" },
        "derived": true,
        "authoritative": false,
        "source": "document/workbook.xlsx",
        "sheet": name,
        "range": ai_context::qualify(name, r0, c0, r1, c1),
        "origin": ai_context::format_cell(r0, c0),
        "shape": [r1 - r0 + 1, c1 - c0 + 1],
        "populated": populated_cells,
        "rows": rows,
    });
    if !display.is_empty() {
        chunk["display"] = Value::Object(display);
    }
    Ok(chunk)
}

fn udoc_include_derived(query: &str) -> bool {
    !["0", "false", "off", "no"].contains(
        &qget(query, "derived")
            .unwrap_or("1")
            .to_ascii_lowercase()
            .as_str(),
    )
}

// 导出 udoc 格式（参考母项目 UDOC3 结构：JSON 新增 unidoc_type:"cell" 标记 UniCell）
fn api_export_udoc(st: &AppState, query: &str) -> Result<Resp, String> {
    let basename = export_basename(st, query);
    let wb = &st.model.get_model().workbook;
    let names = wb.get_worksheet_names();
    let include_derived = udoc_include_derived(query);
    let mut parts: Vec<(String, Vec<u8>, String)> = Vec::new();
    let mut doc_sheets = Vec::new();
    let mut relationships = Vec::new();
    let mut emitted_media = std::collections::HashSet::new();
    for (i, name) in names.iter().enumerate() {
        let sheet = i as u32;
        let chunk_path = format!("document/chunks/sheet{sheet}.json");
        if include_derived {
            parts.push((
                chunk_path.clone(),
                serde_json::to_vec(&udoc_sheet_chunk(st, sheet, name)?)
                    .map_err(|e| e.to_string())?,
                "application/json".to_string(),
            ));
        }
        // 对象媒体外置（图片/视频 DataURL、SVG 文本 → 内容寻址 media）
        let objects = st.objects.get(&sheet).cloned().unwrap_or_default();
        let mut doc_objects = Vec::new();
        for o in &objects {
            let mut oj = o.clone();
            let otype = o["type"].as_str().unwrap_or("");
            let src = o["config"]["src"].as_str().unwrap_or("");
            if (otype == "image" || otype == "video") && src.starts_with("data:") {
                if let Some((mime, bytes)) = parse_data_url(src) {
                    let ext = mime.split('/').nth(1).unwrap_or("bin").to_string();
                    let hash = sha256_hex(&bytes);
                    let mpath = format!("media/{hash}.{ext}");
                    if emitted_media.insert(mpath.clone()) {
                        parts.push((mpath.clone(), bytes, mime.clone()));
                    }
                    relationships.push(json!({"source":format!("sheet{sheet}"),"type":otype,"target":mpath.clone(),"mode":"internal"}));
                    oj["config"]["src"] = json!(mpath);
                }
            } else if otype == "svg" {
                let svg = o["config"]["svg"].as_str().unwrap_or("");
                if !svg.is_empty() {
                    let bytes = svg.as_bytes().to_vec();
                    let hash = sha256_hex(&bytes);
                    let mpath = format!("media/{hash}.svg");
                    if emitted_media.insert(mpath.clone()) {
                        parts.push((mpath.clone(), bytes, "image/svg+xml".to_string()));
                    }
                    relationships.push(json!({"source":format!("sheet{sheet}"),"type":"svg","target":mpath.clone(),"mode":"internal"}));
                    oj["config"]["svg"] = json!(mpath);
                }
            }
            doc_objects.push(oj);
        }
        let merges = st.model.get_merged_cells(sheet).unwrap_or_default();
        let mut sheet_entry = json!({"name":name,"objects":doc_objects,"merges":merges});
        if include_derived {
            sheet_entry["chunk"] = json!(chunk_path);
        }
        doc_sheets.push(sheet_entry);
    }
    // 无损源：内嵌完整 xlsx（导入时直接 load_from_xlsx 还原，保证样式/公式无损）
    let xlsx = model_to_preserved_xlsx_bytes(st)?;
    parts.push((
        "document/workbook.xlsx".into(),
        xlsx,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
    ));
    if include_derived {
        parts.push((
            "document/digest.json".into(),
            serde_json::to_vec(&ai_workbook_digest(st)?).map_err(|e| e.to_string())?,
            "application/json".into(),
        ));
    }
    let document = json!({
        "format":"udoc","version":3,"unidoc_type":"cell","app":"UniCell",
        "authoritativeWorkbook":"document/workbook.xlsx",
        "sheets":doc_sheets
    });
    parts.push((
        "document/document.json".into(),
        serde_json::to_vec(&document).map_err(|e| e.to_string())?,
        "application/json".into(),
    ));
    let rels = json!({"version":1,"relationships":relationships});
    parts.push((
        "rels/relationships.json".into(),
        serde_json::to_vec(&rels).map_err(|e| e.to_string())?,
        "application/json".into(),
    ));
    let mut features = vec![
        "unicell",
        "objects",
        "xlsx",
        "tail-directory",
        "independent-compression",
        "brotli",
        "zip-static-resources",
        "hybrid-br-zip",
        "sha256-integrity",
        "bounded-read",
    ];
    if include_derived {
        features.push("ai-derived-view");
    }
    let manifest = json!({
        "format":"udoc-package","version":3,"unidoc_type":"cell","app":"UniCell",
        "basename":basename,"root":"document/document.json",
        "workbook":"document/workbook.xlsx","authoritative":"document/workbook.xlsx",
        "relationships":"rels/relationships.json","sheetCount":names.len(),
        "features":features,
        "compression": {
            "strategy": "hybrid-br-zip",
            "text": "brotli-q9",
            "binary": "single-entry-zip-store-or-deflate",
            "directory": "brotli-q9",
            "wholeFile": false
        },
        "derivedViews": if include_derived { json!({
            "authoritative": false,
            "source": "document/workbook.xlsx",
            "schema": "unicell-row-major-v2",
            "layouts": ["dense", "sparse"],
            "chunks": "document/chunks/sheet{index}.json",
            "digest": "document/digest.json",
            "purpose": "AI and external-tool context; safe to discard and regenerate"
        }) } else { Value::Null }
    });
    parts.insert(
        0,
        (
            "manifest.json".into(),
            serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
            "application/json".into(),
        ),
    );
    let bytes = encode_udoc3(parts)?;
    let fname = http_safe_attachment_name(&basename, "udoc");
    Ok(tiny_http::Response::from_data(bytes)
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(
                &b"Content-Type"[..],
                &b"application/vnd.unicell.udoc"[..],
            )
            .unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Disposition"[..], fname.as_bytes()).unwrap(),
        ))
}

// Content-Disposition 文件名：主名回退 ASCII，中文等非 ASCII 走 RFC5987 filename*（避免 header 非法字节）
fn http_safe_attachment_name(basename: &str, ext: &str) -> String {
    let ascii: String = basename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii = if ascii.is_empty() {
        "workbook".to_string()
    } else {
        ascii
    };
    let mut enc = String::new();
    for b in basename.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => enc.push(*b as char),
            _ => enc.push_str(&format!("%{:02X}", b)),
        }
    }
    format!("attachment; filename=\"{ascii}.{ext}\"; filename*=UTF-8''{enc}.{ext}")
}

// ---------- xlsx 嵌图：把插入对象以 drawing 图层注入 xlsx(zip) ----------
const EMU_PER_PX: i64 = 9525; // 1 像素 = 9525 EMU

// 对象媒体准备：返回 (media文件名, 字节, 是png)
fn object_media(o: &Value, idx: usize) -> Option<(String, Vec<u8>, bool)> {
    // A native DrawingML object is composed back into the generated drawing later.  The object
    // payload intentionally contains no preview bytes, because the SVG/HTML preview is not the
    // authoritative Excel representation.
    if o.get("nativeDrawing").is_some() {
        return None;
    }
    let otype = o["type"].as_str().unwrap_or("");
    if otype == "svg" {
        let svg = o["svg"].as_str().unwrap_or("");
        if svg.is_empty() {
            return None;
        }
        let emf = svg2emf::svg_to_emf(svg, svg2emf::EmitOptions::default()).ok()?;
        Some((format!("unicellImage{idx}.emf"), emf, false))
    } else {
        // image 原图 / text·html·video 的 canvas 截图（均为 PNG DataURL）
        let png = o["png"].as_str().unwrap_or("");
        let (_, bytes) = parse_data_url(png)?;
        Some((format!("unicellImage{idx}.png"), bytes, true))
    }
}

// 提取“纯公式”单元格的 LaTeX（整格恰为 $$..$$ 或 $..$）；混合文本不转，返回 None
fn extract_pure_latex(content: &str) -> Option<String> {
    let t = content.trim();
    if t.starts_with("$$") && t.ends_with("$$") && t.len() >= 5 {
        let inner = t[2..t.len() - 2].trim();
        if !inner.is_empty() && !inner.contains("$$") {
            return Some(inner.to_string());
        }
    } else if t.starts_with('$') && t.ends_with('$') && t.len() >= 3 && !t.starts_with("$$") {
        let inner = t[1..t.len() - 1].trim();
        if !inner.is_empty() && !inner.contains('$') {
            return Some(inner.to_string());
        }
    }
    None
}

// 批量 LaTeX -> OMML（shell 到 系统 python + tools/latex2omml.py）；失败返回空表（回退纯文本，导出不会报错）
fn latex_to_omml_map(items: &[(String, String)]) -> std::collections::HashMap<String, String> {
    let mut result = std::collections::HashMap::new();
    if items.is_empty() {
        return result;
    }
    let indata: Vec<Value> = items
        .iter()
        .map(|(k, l)| json!({ "key": k, "latex": l }))
        .collect();
    let tmp_in = std::env::temp_dir().join("unicell_latex_in.json");
    let tmp_out = std::env::temp_dir().join("unicell_latex_out.json");
    if std::fs::write(&tmp_in, serde_json::to_vec(&indata).unwrap_or_default()).is_err() {
        return result;
    }
    let _ = std::fs::remove_file(&tmp_out);
    let script = [
        "tools/latex2omml.py",
        "../tools/latex2omml.py",
        "../../tools/latex2omml.py",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .find(|p| p.exists());
    let Some(script) = script else {
        return result;
    };
    // python 候选：环境变量 UNICELL_PYTHON > 系统 python
    let mut pys: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("UNICELL_PYTHON") {
        if !p.is_empty() {
            pys.push(p);
        }
    }
    pys.push("python".to_string());
    for py in pys {
        let out = std::process::Command::new(&py)
            .arg(&script)
            .arg(&tmp_in)
            .arg(&tmp_out)
            .output();
        if let Ok(o) = out {
            if o.status.success() {
                if let Ok(bytes) = std::fs::read(&tmp_out) {
                    if let Ok(map) =
                        serde_json::from_slice::<std::collections::HashMap<String, Value>>(&bytes)
                    {
                        for (k, v) in map {
                            if let Some(s) = v.as_str() {
                                result.insert(k, s.to_string());
                            }
                        }
                        return result;
                    }
                }
            }
        }
    }
    result
}

// 扫描全表“纯公式”单元格 → 生成 OMML 公式形状描述（供 inject_drawings 注入）
fn build_equations(st: &AppState) -> Vec<Value> {
    let wb = &st.model.get_model().workbook;
    let names = wb.get_worksheet_names();
    let mut items: Vec<(String, String)> = Vec::new();
    let mut meta: Vec<(String, u32, i32, i32, String)> = Vec::new();
    for (i, _n) in names.iter().enumerate() {
        let sheet = i as u32;
        let Ok(ws) = wb.worksheet(sheet) else {
            continue;
        };
        let d = ws.dimension();
        for r in d.min_row..=d.max_row {
            for c in d.min_column..=d.max_column {
                let content = st.model.get_cell_content(sheet, r, c).unwrap_or_default();
                if let Some(latex) = extract_pure_latex(&content) {
                    let key = format!("{sheet}_{r}_{c}");
                    items.push((key.clone(), latex));
                    meta.push((key, sheet, r, c, content));
                }
            }
        }
    }
    if items.is_empty() {
        return Vec::new();
    }
    let omml = latex_to_omml_map(&items);
    let mut eqs = Vec::new();
    for (key, sheet, r, c, fallback) in meta {
        if let Some(o) = omml.get(&key) {
            let w = ((fallback.chars().count() as i64) * 9).clamp(140, 640);
            eqs.push(json!({ "sheet": sheet, "r": r, "c": c, "w": w, "h": 40, "omml": o, "fallback": fallback }));
        }
    }
    eqs
}

// r,c(1-based) -> A1 式单元格引用
fn cell_ref(r: i32, c: i32) -> String {
    let mut col = String::new();
    let mut n = c;
    while n > 0 {
        let rem = (n - 1) % 26;
        col.insert(0, (b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    format!("{col}{r}")
}

// 从 sheet XML 中移除指定单元格的 <c r="REF" ...>..</c>（公式单元格不写 LaTeX 文本，Excel 约定）
fn remove_cell_from_sheet(xml: &str, cref: &str) -> String {
    let needle = format!("<c r=\"{cref}\"");
    let Some(start) = xml.find(&needle) else {
        return xml.to_string();
    };
    let after = &xml[start..];
    let Some(gt) = after.find('>') else {
        return xml.to_string();
    };
    // 自闭合 <c .../> 或 带内容 <c ...>..</c>
    if after.as_bytes()[gt - 1] == b'/' {
        let end = start + gt + 1;
        format!("{}{}", &xml[..start], &xml[end..])
    } else if let Some(close) = after.find("</c>") {
        let end = start + close + 4;
        format!("{}{}", &xml[..start], &xml[end..])
    } else {
        xml.to_string()
    }
}

// ---------- xlsx 导入：提取内嵌 drawing 图片为 UniCell 对象（IronCalc 不导入 drawing，这里补齐） ----------
fn xml_attr(s: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let p = s.find(&key)? + key.len();
    let e = s[p..].find('"')? + p;
    Some(s[p..e].to_string())
}
fn xml_tag_i64(block: &str, tag: &str) -> Option<i64> {
    let open = format!("<{tag}>");
    let s = block.find(&open)? + open.len();
    let close = format!("</{tag}>");
    let e = block[s..].find(&close)? + s;
    block[s..e].trim().parse().ok()
}
fn xml_elem_attr_i64(block: &str, elem: &str, attr: &str) -> Option<i64> {
    let p = block.find(&format!("<{elem}"))?;
    let seg = &block[p..];
    let gt = seg.find('>')?;
    xml_attr(&seg[..gt], attr).and_then(|v| v.parse().ok())
}
fn path_dir(p: &str) -> String {
    match p.rsplit_once('/') {
        Some((d, _)) => format!("{d}/"),
        None => String::new(),
    }
}
fn resolve_rel_path(base_dir: &str, target: &str) -> String {
    let mut parts: Vec<String> = if target.starts_with('/') {
        Vec::new()
    } else {
        base_dir
            .trim_end_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    };
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            _ => parts.push(seg.to_string()),
        }
    }
    parts.join("/")
}
fn parse_rels_map(xml: &str) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    for part in xml.split("<Relationship ").skip(1) {
        if let (Some(id), Some(t)) = (xml_attr(part, "Id"), xml_attr(part, "Target")) {
            m.insert(id, t);
        }
    }
    m
}
fn workbook_sheet_paths(files: &std::collections::HashMap<String, Vec<u8>>) -> Vec<String> {
    const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let paths = (|| {
        let workbook = std::str::from_utf8(files.get("xl/workbook.xml")?).ok()?;
        let rels_xml = std::str::from_utf8(files.get("xl/_rels/workbook.xml.rels")?).ok()?;
        let rels = parse_rels_map(rels_xml);
        let document = roxmltree::Document::parse(workbook).ok()?;
        let result: Vec<String> = document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "sheet")
            .filter_map(|node| {
                node.attributes().find(|attribute| {
                    attribute.name() == "id" && attribute.namespace() == Some(REL_NS)
                })
            })
            .filter_map(|attribute| rels.get(attribute.value()))
            .map(|target| resolve_rel_path("xl/", target))
            .filter(|path| files.contains_key(path))
            .collect();
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    })();
    if let Some(paths) = paths {
        return paths;
    }
    let mut fallback: Vec<String> = files
        .keys()
        .filter(|name| {
            name.starts_with("xl/worksheets/")
                && name.ends_with(".xml")
                && !name.contains("/_rels/")
        })
        .cloned()
        .collect();
    fallback.sort();
    fallback
}

fn snapshot_workbook_sheet_paths(snapshot: &OpcPackageSnapshot) -> Vec<String> {
    // workbook_sheet_paths only needs the two workbook XML parts plus membership checks for the
    // sheet targets.  Avoid cloning a potentially hundreds-of-megabytes OPC snapshot.
    let mut lookup = std::collections::HashMap::new();
    for name in ["xl/workbook.xml", "xl/_rels/workbook.xml.rels"] {
        if let Some(bytes) = snapshot.parts.get(name) {
            lookup.insert(name.to_string(), bytes.clone());
        }
    }
    for name in snapshot.parts.keys().filter(|name| {
        name.starts_with("xl/worksheets/") && name.ends_with(".xml") && !name.contains("/_rels/")
    }) {
        lookup.entry(name.clone()).or_default();
    }
    workbook_sheet_paths(&lookup)
}
fn import_drawings_objects(
    xlsx: &[u8],
    model: &UserModel,
) -> std::collections::HashMap<u32, Vec<Value>> {
    use std::io::Read;
    let mut result: std::collections::HashMap<u32, Vec<Value>> = std::collections::HashMap::new();
    let Ok(mut za) = zip::read::ZipArchive::new(std::io::Cursor::new(xlsx.to_vec())) else {
        return result;
    };
    let mut files: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
    for i in 0..za.len() {
        if let Ok(mut f) = za.by_index(i) {
            let name = f.name().to_string();
            let mut b = Vec::new();
            if f.read_to_end(&mut b).is_ok() {
                files.insert(name, b);
            }
        }
    }
    for (sheet_index, sheet_file) in workbook_sheet_paths(&files).into_iter().enumerate() {
        let si = sheet_index as u32;
        let rels_file = drawing_relationships_path(&sheet_file);
        let sheet_drawing_id = files.get(&sheet_file).and_then(|bytes| {
            let xml = String::from_utf8_lossy(bytes);
            xml_element_attr(&xml, "drawing", "r:id")
        });
        let drawing_path = files.get(&rels_file).and_then(|b| {
            let xml = String::from_utf8_lossy(b);
            for part in xml.split("<Relationship ").skip(1) {
                let matches_id = sheet_drawing_id
                    .as_deref()
                    .map(|wanted| xml_attr(part, "Id").as_deref() == Some(wanted))
                    .unwrap_or(true);
                if matches_id && part.contains("relationships/drawing") {
                    if let Some(t) = xml_attr(part, "Target") {
                        return Some(resolve_rel_path(&path_dir(&sheet_file), &t));
                    }
                }
            }
            None
        });
        if let Some(dpath) = drawing_path {
            if let Some(db) = files.get(&dpath) {
                let dxml = String::from_utf8_lossy(db).to_string();
                let drels_path = drawing_relationships_path(&dpath);
                let rels = files
                    .get(&drels_path)
                    .map(|b| parse_rels_map(&String::from_utf8_lossy(b)))
                    .unwrap_or_default();
                let objs = parse_drawing_pics(&dxml, &rels, &dpath, &files, si, model);
                if !objs.is_empty() {
                    result.insert(si, objs);
                }
            }
        }
    }
    result
}
fn xml_element_attr(block: &str, elem: &str, attr: &str) -> Option<String> {
    let p = block.find(&format!("<{elem}"))?;
    let seg = &block[p..];
    let gt = seg.find('>')?;
    xml_attr(&seg[..gt], attr)
}
fn xml_local_tag_i64(block: &str, local: &str) -> Option<i64> {
    xml_tag_i64(block, &format!("xdr:{local}")).or_else(|| xml_tag_i64(block, local))
}
fn xml_local_elem_attr_i64(block: &str, local: &str, attr: &str) -> Option<i64> {
    xml_elem_attr_i64(block, &format!("xdr:{local}"), attr)
        .or_else(|| xml_elem_attr_i64(block, local, attr))
}
fn xml_local_element_attr(block: &str, local: &str, attr: &str) -> Option<String> {
    xml_element_attr(block, &format!("c:{local}"), attr)
        .or_else(|| xml_element_attr(block, local, attr))
}
fn xml_element_block<'a>(block: &'a str, elem: &str) -> Option<&'a str> {
    let p = block.find(&format!("<{elem}"))?;
    let open_end = block[p..].find('>')? + p + 1;
    let close = format!("</{elem}>");
    let end = block[open_end..].find(&close)? + open_end;
    Some(&block[open_end..end])
}
fn xml_local_element_block<'a>(block: &'a str, local: &str) -> Option<&'a str> {
    xml_element_block(block, &format!("xdr:{local}")).or_else(|| xml_element_block(block, local))
}
fn marker_abs(block: &str, sheet: u32, model: &UserModel) -> (i64, i64, i64, i64) {
    let col = xml_local_tag_i64(block, "col").unwrap_or(0);
    let row = xml_local_tag_i64(block, "row").unwrap_or(0);
    let coloff = xml_local_tag_i64(block, "colOff").unwrap_or(0);
    let rowoff = xml_local_tag_i64(block, "rowOff").unwrap_or(0);
    let mut x = 0.0;
    for c in 1..=(col as i32) {
        x += model.get_column_width(sheet, c).unwrap_or(100.0);
    }
    x += coloff as f64 / EMU_PER_PX as f64;
    let mut y = 0.0;
    for r in 1..=(row as i32) {
        y += model.get_row_height(sheet, r).unwrap_or(21.0);
    }
    y += rowoff as f64 / EMU_PER_PX as f64;
    (x.round() as i64, y.round() as i64, row, col)
}
fn drawing_geometry(block: &str, sheet: u32, model: &UserModel) -> (i64, i64, i64, i64, i64, i64) {
    let px = |emu: i64| (emu as f64 / EMU_PER_PX as f64).round() as i64;
    if block.starts_with("<xdr:absoluteAnchor") || block.starts_with("<absoluteAnchor") {
        let x = xml_local_elem_attr_i64(block, "pos", "x")
            .map(px)
            .unwrap_or(0);
        let y = xml_local_elem_attr_i64(block, "pos", "y")
            .map(px)
            .unwrap_or(0);
        let w = xml_local_elem_attr_i64(block, "ext", "cx")
            .map(px)
            .unwrap_or(96)
            .max(16);
        let h = xml_local_elem_attr_i64(block, "ext", "cy")
            .map(px)
            .unwrap_or(96)
            .max(16);
        return (x, y, w, h, 0, 0);
    }
    let from = xml_local_element_block(block, "from").unwrap_or(block);
    let (x, y, row, col) = marker_abs(from, sheet, model);
    if block.starts_with("<xdr:twoCellAnchor") || block.starts_with("<twoCellAnchor") {
        if let Some(to) = xml_local_element_block(block, "to") {
            let (x2, y2, _, _) = marker_abs(to, sheet, model);
            return (x, y, (x2 - x).max(16), (y2 - y).max(16), row, col);
        }
    }
    let w = xml_local_elem_attr_i64(block, "ext", "cx")
        .map(px)
        .unwrap_or(96)
        .max(16);
    let h = xml_local_elem_attr_i64(block, "ext", "cy")
        .map(px)
        .unwrap_or(96)
        .max(16);
    (x, y, w, h, row, col)
}

fn chart_series_color(ser: roxmltree::Node<'_, '_>, theme: &Theme, fallback: &str) -> String {
    let line_like = ser
        .ancestors()
        .find(|node| {
            node.is_element()
                && matches!(
                    node.tag_name().name(),
                    "barChart"
                        | "lineChart"
                        | "pieChart"
                        | "pie3DChart"
                        | "doughnutChart"
                        | "areaChart"
                        | "scatterChart"
                        | "bubbleChart"
                        | "radarChart"
                        | "stockChart"
                )
        })
        .map(|plot| {
            matches!(
                plot.tag_name().name(),
                "lineChart" | "scatterChart" | "radarChart" | "stockChart"
            )
        })
        .unwrap_or(false);
    let Some(shape) = ser
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "spPr")
    else {
        return fallback.to_string();
    };
    let target = if line_like {
        shape
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "ln")
            .unwrap_or(shape)
    } else {
        shape
    };
    let Some(solid) = target
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "solidFill")
    else {
        return fallback.to_string();
    };
    let Some(color) = solid.children().find(|node| {
        node.is_element()
            && matches!(
                node.tag_name().name(),
                "srgbClr" | "schemeClr" | "sysClr" | "prstClr" | "scrgbClr" | "hslClr"
            )
    }) else {
        return fallback.to_string();
    };
    native_shape_edit::resolve_drawing_color(color, theme)
        .map(|resolved| resolved.0)
        // An unknown vendor colour remains in OOXML. Use a neutral preview instead of lying that
        // it was Office accent1; a genuinely unstyled series still uses the chart palette above.
        .unwrap_or_else(|| "#808080".to_string())
}
fn chart_cached_points(node: roxmltree::Node<'_, '_>) -> Vec<String> {
    let mut pts: Vec<(usize, String)> = node
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "pt")
        .filter_map(|pt| {
            let idx = pt
                .attribute("idx")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            let value = pt
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "v")
                .and_then(|n| n.text())
                .unwrap_or("")
                .to_string();
            if value.is_empty() {
                None
            } else {
                Some((idx, value))
            }
        })
        .collect();
    pts.sort_by_key(|p| p.0);
    pts.into_iter().map(|p| p.1).collect()
}
fn chart_node_text(node: roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.is_element() && matches!(n.tag_name().name(), "t" | "v"))
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join("")
}
fn parse_chart_a1(value: &str) -> Option<(i32, i32)> {
    let clean = value.trim().replace('$', "");
    let split = clean.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = clean.split_at(split);
    if letters.is_empty() || digits.is_empty() || !letters.chars().all(|c| c.is_ascii_alphabetic())
    {
        return None;
    }
    let mut col = 0i32;
    for ch in letters.bytes() {
        col = col
            .checked_mul(26)?
            .checked_add((ch.to_ascii_uppercase() - b'A' + 1) as i32)?;
    }
    let row = digits.parse::<i32>().ok()?;
    if row < 1 || col < 1 {
        None
    } else {
        Some((row, col))
    }
}
fn chart_ref_values(
    node: roxmltree::Node<'_, '_>,
    model: &UserModel,
    current_sheet: u32,
    numeric: bool,
) -> Vec<String> {
    let Some(formula) = node
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "f")
        .and_then(|n| n.text())
    else {
        return Vec::new();
    };
    if formula.contains('[') {
        return Vec::new();
    }
    let (sheet_index, range) = if let Some((sheet_name, range)) = formula.rsplit_once('!') {
        let sheet_name = sheet_name.trim().trim_matches('\'').replace("''", "'");
        let names = model.get_model().workbook.get_worksheet_names();
        let Some(index) = names.iter().position(|n| n == &sheet_name) else {
            return Vec::new();
        };
        (index as u32, range)
    } else {
        (current_sheet, formula)
    };
    let (start, end) = range.split_once(':').unwrap_or((range, range));
    let Some((r0, c0)) = parse_chart_a1(start) else {
        return Vec::new();
    };
    let Some((r1, c1)) = parse_chart_a1(end) else {
        return Vec::new();
    };
    if r1 < r0 || c1 < c0 || (r1 - r0 + 1) as i64 * (c1 - c0 + 1) as i64 > 100_000 {
        return Vec::new();
    }
    let mut values = Vec::new();
    for r in r0..=r1 {
        for c in c0..=c1 {
            if numeric {
                let value = model
                    .get_model()
                    .get_cell_value_by_index(sheet_index, r, c)
                    .ok();
                values.push(match value {
                    Some(CellValue::Number(n)) => n.to_string(),
                    Some(CellValue::Boolean(v)) => {
                        if v {
                            "1".into()
                        } else {
                            "0".into()
                        }
                    }
                    Some(CellValue::String(v)) => v
                        .parse::<f64>()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|_| "0".into()),
                    _ => "0".into(),
                });
            } else {
                values.push(
                    model
                        .get_formatted_cell_value(sheet_index, r, c)
                        .unwrap_or_default(),
                );
            }
        }
    }
    values
}
fn chart_xml_to_html(
    chart_xml: &str,
    theme: &Theme,
    source: Option<(&UserModel, u32)>,
) -> Option<(String, String, String)> {
    let doc = roxmltree::Document::parse(chart_xml).ok()?;
    let chart_names = [
        "barChart",
        "lineChart",
        "pieChart",
        "pie3DChart",
        "doughnutChart",
        "areaChart",
        "scatterChart",
        "bubbleChart",
        "radarChart",
        "stockChart",
    ];
    let chart_node = doc
        .descendants()
        .find(|n| n.is_element() && chart_names.contains(&n.tag_name().name()))?;
    let chart_type = match chart_node.tag_name().name() {
        "lineChart" | "stockChart" | "radarChart" => "line",
        "pieChart" | "pie3DChart" | "doughnutChart" => "pie",
        "areaChart" => "area",
        "scatterChart" | "bubbleChart" => "scatter",
        _ => "bar",
    }
    .to_string();
    let title = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "title")
        .map(chart_node_text)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Excel 图表".to_string());
    let palette = [
        &theme.accent1,
        &theme.accent2,
        &theme.accent3,
        &theme.accent4,
        &theme.accent5,
        &theme.accent6,
    ];
    let mut series_json = Vec::new();
    for (i, ser) in chart_node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "ser")
        .enumerate()
    {
        let tx = ser
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "tx");
        let mut name = tx
            .map(chart_node_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        if name.is_empty() {
            if let (Some(tx), Some((model, current_sheet))) = (tx, source) {
                name = chart_ref_values(tx, model, current_sheet, false)
                    .into_iter()
                    .next()
                    .unwrap_or_default();
            }
        }
        if name.is_empty() {
            name = format!("系列 {}", i + 1);
        }
        let cat = ser
            .children()
            .find(|n| n.is_element() && matches!(n.tag_name().name(), "cat" | "xVal"));
        let mut categories = cat.map(chart_cached_points).unwrap_or_default();
        if categories.is_empty() {
            if let (Some(cat), Some((model, current_sheet))) = (cat, source) {
                categories = chart_ref_values(cat, model, current_sheet, false);
            }
        }
        let val = ser
            .children()
            .find(|n| n.is_element() && matches!(n.tag_name().name(), "val" | "yVal"));
        let mut raw_values = val.map(chart_cached_points).unwrap_or_default();
        if raw_values.is_empty() {
            if let (Some(val), Some((model, current_sheet))) = (val, source) {
                raw_values = chart_ref_values(val, model, current_sheet, true);
            }
        }
        let values = raw_values
            .into_iter()
            .map(|v| v.parse::<f64>().unwrap_or(0.0))
            .collect::<Vec<_>>();
        let fallback = palette[i % palette.len()];
        series_json.push(json!({"name":name,"categories":categories,"values":values,"color":chart_series_color(ser, theme, fallback)}));
    }
    let data = json!({"title":title,"type":chart_type,"series":series_json});
    let payload = serde_json::to_string(&data)
        .ok()?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    let template = r#"<!doctype html><html><head><meta charset="utf-8"><style>html,body{margin:0;width:100%;height:100%;overflow:hidden;background:#fff;font:12px "Segoe UI",Arial,sans-serif;color:#222}#chart{width:100%;height:100%;display:block}</style></head><body><canvas id="chart"></canvas><script id="chart-data" type="application/json">__CHART_DATA__</script><script>
(()=>{const d=JSON.parse(document.getElementById('chart-data').textContent),c=document.getElementById('chart'),x=c.getContext('2d');const draw=()=>{const q=devicePixelRatio||1,w=Math.max(120,innerWidth),h=Math.max(90,innerHeight);c.width=w*q;c.height=h*q;c.style.width=w+'px';c.style.height=h+'px';x.setTransform(q,0,0,q,0,0);x.clearRect(0,0,w,h);x.fillStyle='#fff';x.fillRect(0,0,w,h);x.fillStyle='#222';x.font='600 14px Segoe UI,Arial';x.textAlign='center';x.fillText(d.title||'Excel 图表',w/2,21);const s=d.series||[],L=48,R=18,T=38,B=34,pw=w-L-R,ph=h-T-B;if(!s.length||!s.some(z=>z.values&&z.values.length)){x.fillStyle='#777';x.font='12px Segoe UI';x.fillText('图表数据缓存为空（已保留原始 Excel chart XML）',w/2,h/2);return}if(d.type==='pie'){const z=s[0],v=z.values||[],sum=v.reduce((a,b)=>a+Math.max(0,b),0)||1;let a=-Math.PI/2;v.forEach((n,i)=>{const da=Math.max(0,n)/sum*Math.PI*2;x.beginPath();x.moveTo(w/2,h/2+8);x.arc(w/2,h/2+8,Math.max(18,Math.min(pw,ph)*.38),a,a+da);x.closePath();x.fillStyle=(s[i]&&s[i].color)||z.color||'#4472C4';x.fill();a+=da});return}const all=s.flatMap(z=>z.values||[]),mn=Math.min(0,...all),mx=Math.max(1,...all),span=mx-mn||1,y=n=>T+ph-(n-mn)/span*ph;x.strokeStyle='#d9d9d9';x.lineWidth=1;for(let i=0;i<=4;i++){const yy=T+ph*i/4;x.beginPath();x.moveTo(L,yy);x.lineTo(w-R,yy);x.stroke();x.fillStyle='#666';x.font='10px Segoe UI';x.textAlign='right';x.fillText((mx-span*i/4).toLocaleString(),L-5,yy+3)}x.strokeStyle='#888';x.beginPath();x.moveTo(L,T);x.lineTo(L,T+ph);x.lineTo(w-R,T+ph);x.stroke();const N=Math.max(1,...s.map(z=>(z.values||[]).length));if(d.type==='bar'){const group=pw/N,bw=Math.max(2,group*.72/Math.max(1,s.length));s.forEach((z,j)=>(z.values||[]).forEach((n,i)=>{x.fillStyle=z.color||'#4472C4';x.fillRect(L+i*group+group*.14+j*bw,y(Math.max(n,0)),bw,Math.abs(y(n)-y(0)))}))}else{s.forEach(z=>{const v=z.values||[];x.beginPath();v.forEach((n,i)=>{const xx=L+(N===1?pw/2:i*pw/(N-1));i?x.lineTo(xx,y(n)):x.moveTo(xx,y(n))});if(d.type==='area'){x.lineTo(L+pw,y(0));x.lineTo(L,y(0));x.closePath();x.globalAlpha=.25;x.fillStyle=z.color||'#4472C4';x.fill();x.globalAlpha=1}x.strokeStyle=z.color||'#4472C4';x.lineWidth=2;x.stroke()})}const cats=(s[0]&&s[0].categories)||[];x.fillStyle='#555';x.font='10px Segoe UI';x.textAlign='center';const step=Math.max(1,Math.ceil(N/8));for(let i=0;i<N;i+=step)x.fillText(String(cats[i]??i+1).slice(0,14),L+(N===1?pw/2:i*pw/Math.max(1,N-1)),h-12)};new ResizeObserver(draw).observe(document.body);draw()})();
</script></body></html>"#;
    Some((
        template.replace("__CHART_DATA__", &payload),
        chart_type,
        title,
    ))
}

fn drawing_anchor_slices(dxml: &str) -> Vec<(usize, usize, String)> {
    let mut starts: Vec<(usize, String)> = Vec::new();
    for (needle, kind) in [
        ("<xdr:oneCellAnchor", "oneCellAnchor"),
        ("<xdr:twoCellAnchor", "twoCellAnchor"),
        ("<xdr:absoluteAnchor", "absoluteAnchor"),
        ("<oneCellAnchor", "oneCellAnchor"),
        ("<twoCellAnchor", "twoCellAnchor"),
        ("<absoluteAnchor", "absoluteAnchor"),
    ] {
        let mut offset = 0usize;
        while let Some(relative) = dxml[offset..].find(needle) {
            let start = offset + relative;
            starts.push((start, kind.to_string()));
            offset = start + needle.len();
        }
    }
    starts.sort_by_key(|item| item.0);
    starts.dedup_by_key(|item| item.0);
    starts
        .iter()
        .enumerate()
        .map(|(index, (start, kind))| {
            let limit = starts
                .get(index + 1)
                .map(|item| item.0)
                .unwrap_or(dxml.len());
            let tail = &dxml[*start..limit];
            let prefixed = format!("</xdr:{kind}>");
            let bare = format!("</{kind}>");
            let end = tail
                .find(&prefixed)
                .map(|p| start + p + prefixed.len())
                .or_else(|| tail.find(&bare).map(|p| start + p + bare.len()))
                .unwrap_or(limit);
            (*start, end, kind.clone())
        })
        .collect()
}

fn drawing_any_element_attr(block: &str, local: &str, attr: &str) -> Option<String> {
    for prefix in ["xdr", "a", "c", "dgm", "dsp", "pic"] {
        if let Some(value) = xml_element_attr(block, &format!("{prefix}:{local}"), attr) {
            return Some(value);
        }
    }
    xml_element_attr(block, local, attr)
}

fn xml_has_opening_local(block: &str, wanted: &str) -> bool {
    let bytes = block.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let Some(relative) = block[cursor..].find('<') else {
            break;
        };
        cursor += relative + 1;
        if cursor >= bytes.len() {
            break;
        }
        if matches!(bytes[cursor], b'/' | b'!' | b'?') {
            continue;
        }
        let start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b'/' | b'>')
        {
            cursor += 1;
        }
        let qualified = &block[start..cursor];
        if qualified
            .rsplit_once(':')
            .map(|(_, local)| local)
            .unwrap_or(qualified)
            == wanted
        {
            return true;
        }
    }
    false
}

fn drawing_native_kind(block: &str) -> &'static str {
    if xml_has_opening_local(block, "chart") {
        "chart"
    } else if xml_has_opening_local(block, "relIds") || block.contains("/diagram") {
        "smartart"
    } else if xml_has_opening_local(block, "timeslicer") {
        "timeline"
    } else if xml_has_opening_local(block, "slicer") {
        "slicer"
    } else if xml_has_opening_local(block, "pic") {
        "picture"
    } else if xml_has_opening_local(block, "grpSp") {
        "group"
    } else if xml_has_opening_local(block, "cxnSp") {
        "connector"
    } else if xml_has_opening_local(block, "sp") {
        "shape"
    } else if xml_has_opening_local(block, "graphicFrame") {
        "graphicFrame"
    } else {
        "drawing"
    }
}

fn drawing_fragment_document(block: &str) -> String {
    drawing_fragment_document_with_source(block, None)
}

fn drawing_fragment_document_with_source(block: &str, source: Option<&str>) -> String {
    let mut namespaces: std::collections::BTreeMap<String, String> = [
        (
            "xdr",
            "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing",
        ),
        ("a", "http://schemas.openxmlformats.org/drawingml/2006/main"),
        (
            "r",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
        ),
        (
            "c",
            "http://schemas.openxmlformats.org/drawingml/2006/chart",
        ),
        (
            "dgm",
            "http://schemas.openxmlformats.org/drawingml/2006/diagram",
        ),
        (
            "dsp",
            "http://schemas.microsoft.com/office/drawing/2008/diagram",
        ),
        (
            "mc",
            "http://schemas.openxmlformats.org/markup-compatibility/2006",
        ),
        (
            "a14",
            "http://schemas.microsoft.com/office/drawing/2010/main",
        ),
        (
            "a15",
            "http://schemas.microsoft.com/office/drawing/2012/main",
        ),
        (
            "a16",
            "http://schemas.microsoft.com/office/drawing/2014/main",
        ),
        (
            "sle",
            "http://schemas.microsoft.com/office/drawing/2010/slicer",
        ),
        (
            "tsle",
            "http://schemas.microsoft.com/office/drawing/2012/timeslicer",
        ),
    ]
    .into_iter()
    .map(|(prefix, uri)| (prefix.to_string(), uri.to_string()))
    .collect();
    if let Some(source) = source {
        if let Ok(document) = roxmltree::Document::parse(source) {
            for namespace in document.root_element().namespaces() {
                namespaces.insert(
                    namespace.name().unwrap_or("").to_string(),
                    namespace.uri().to_string(),
                );
            }
        }
    }
    let declarations = namespaces
        .into_iter()
        .map(|(prefix, uri)| {
            if prefix.is_empty() {
                format!(" xmlns=\"{}\"", html_escape(&uri))
            } else {
                format!(" xmlns:{}=\"{}\"", prefix, html_escape(&uri))
            }
        })
        .collect::<String>();
    format!("<root{declarations}>{block}</root>")
}

fn drawing_color(node: roxmltree::Node<'_, '_>, theme: &Theme) -> (String, f64) {
    native_shape_edit::resolve_drawing_color(node, theme)
        // Unknown/placeholder colours remain visibly unresolved instead of being mistaken for
        // Office accent1.  The original DrawingML token is still retained for Excel export.
        .unwrap_or_else(|| ("#808080".to_string(), 1.0))
}

fn drawing_text_from_part(bytes: &[u8]) -> Vec<String> {
    let Ok(xml) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    let Ok(document) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    document
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "t")
        .filter_map(|n| n.text().map(str::trim))
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .collect()
}

fn drawing_preview_svg(
    block: &str,
    kind: &str,
    rels: &std::collections::HashMap<String, String>,
    dpath: &str,
    files: &std::collections::HashMap<String, Vec<u8>>,
    theme: &Theme,
) -> String {
    let wrapped = drawing_fragment_document(block);
    let document = roxmltree::Document::parse(&wrapped).ok();
    let mut texts: Vec<String> = document
        .as_ref()
        .map(|doc| {
            doc.descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "t")
                .filter_map(|n| n.text().map(str::trim))
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if kind == "smartart" {
        for attr in ["r:dm", "dm"] {
            if let Some(rid) = xml_attr(block, attr) {
                if let Some(target) = rels.get(&rid) {
                    let path = resolve_rel_path(&path_dir(dpath), target);
                    if let Some(bytes) = files.get(&path) {
                        texts.extend(drawing_text_from_part(bytes));
                    }
                }
            }
        }
    }
    texts.dedup();
    let name = drawing_any_element_attr(block, "cNvPr", "name").unwrap_or_else(|| {
        match kind {
            "smartart" => "SmartArt",
            "slicer" => "Slicer",
            "timeline" => "Timeline",
            "group" => "DrawingML group",
            "connector" => "Connector",
            "shape" => "DrawingML shape",
            _ => "DrawingML object",
        }
        .to_string()
    });
    let label = if texts.is_empty() {
        name
    } else {
        texts.into_iter().take(6).collect::<Vec<_>>().join(" · ")
    };
    let mut defs = String::new();
    let mut fill = "#EAF2F8".to_string();
    let mut fill_opacity = 1.0f64;
    let mut geometry = "rect";
    if let Some(doc) = document.as_ref() {
        if let Some(prst) = doc
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "prstGeom")
            .and_then(|n| n.attribute("prst"))
        {
            geometry = prst;
        }
        if let Some(solid) = doc
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "solidFill")
        {
            if let Some(color) = solid.children().find(|n| {
                n.is_element()
                    && matches!(
                        n.tag_name().name(),
                        "srgbClr" | "schemeClr" | "sysClr" | "prstClr" | "scrgbClr" | "hslClr"
                    )
            }) {
                let resolved = drawing_color(color, theme);
                fill = resolved.0;
                fill_opacity = resolved.1;
            }
        }
        if let Some(gradient) = doc
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "gradFill")
        {
            let mut stops = String::new();
            for stop in gradient
                .descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "gs")
            {
                let offset = stop
                    .attribute("pos")
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0)
                    / 1000.0;
                if let Some(color) = stop.children().find(|n| {
                    n.is_element()
                        && matches!(
                            n.tag_name().name(),
                            "srgbClr" | "schemeClr" | "sysClr" | "prstClr" | "scrgbClr" | "hslClr"
                        )
                }) {
                    let (rgb, opacity) = drawing_color(color, theme);
                    stops.push_str(&format!(
                        "<stop offset=\"{}%\" stop-color=\"{}\" stop-opacity=\"{}\"/>",
                        offset.clamp(0.0, 100.0),
                        rgb,
                        opacity
                    ));
                }
            }
            if !stops.is_empty() {
                let angle = gradient
                    .descendants()
                    .find(|n| n.is_element() && n.tag_name().name() == "lin")
                    .and_then(|n| n.attribute("ang"))
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0)
                    / 60_000.0;
                defs = format!(
                    "<defs><linearGradient id=\"nativeGradient\" x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\" gradientTransform=\"rotate({angle} .5 .5)\">{stops}</linearGradient></defs>"
                );
                fill = "url(#nativeGradient)".to_string();
                fill_opacity = 1.0;
            }
        }
    }
    let shape = match geometry {
        "ellipse" | "arc" => format!("<ellipse cx=\"500\" cy=\"300\" rx=\"475\" ry=\"275\" fill=\"{fill}\" fill-opacity=\"{fill_opacity}\" stroke=\"#667085\" stroke-width=\"8\"/>"),
        "line" | "straightConnector1" => "<line x1=\"35\" y1=\"565\" x2=\"965\" y2=\"35\" stroke=\"#4472C4\" stroke-width=\"14\"/>".to_string(),
        "roundRect" => format!("<rect x=\"25\" y=\"25\" width=\"950\" height=\"550\" rx=\"70\" fill=\"{fill}\" fill-opacity=\"{fill_opacity}\" stroke=\"#667085\" stroke-width=\"8\"/>"),
        _ => format!("<rect x=\"25\" y=\"25\" width=\"950\" height=\"550\" fill=\"{fill}\" fill-opacity=\"{fill_opacity}\" stroke=\"#667085\" stroke-width=\"8\"/>"),
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1000 600\" preserveAspectRatio=\"none\">{defs}{shape}<text x=\"500\" y=\"310\" text-anchor=\"middle\" dominant-baseline=\"middle\" font-family=\"Calibri,Segoe UI,sans-serif\" font-size=\"54\" fill=\"#1F2937\">{}</text></svg>",
        html_escape(&label)
    )
}

fn drawing_native_descriptor(
    dpath: &str,
    anchor_index: usize,
    anchor_kind: &str,
    kind: &str,
    block: &str,
    geometry: (i64, i64, i64, i64, i64, i64),
) -> Value {
    let non_visual_id = drawing_any_element_attr(block, "cNvPr", "id").unwrap_or_default();
    let name = drawing_any_element_attr(block, "cNvPr", "name").unwrap_or_default();
    let token_hash = sha256_hex(format!("{dpath}|{anchor_index}|{non_visual_id}").as_bytes());
    let (x, y, w, h, row, col) = geometry;
    json!({
        "token": format!("native-{}", &token_hash[..24]), "drawingPath": dpath,
        "anchorIndex": anchor_index, "anchorKind": anchor_kind, "kind": kind,
        "nonVisualId": non_visual_id, "name": name,
        "baseline": {"mode":"abs","x":x,"y":y,"w":w,"h":h,"r":row + 1,"c":col + 1}
    })
}

fn drawing_theme_model(theme: &Theme) -> Value {
    json!({
        "name": theme.name,
        "dk1": theme.dk1,
        "lt1": theme.lt1,
        "dk2": theme.dk2,
        "lt2": theme.lt2,
        "accent1": theme.accent1,
        "accent2": theme.accent2,
        "accent3": theme.accent3,
        "accent4": theme.accent4,
        "accent5": theme.accent5,
        "accent6": theme.accent6,
        "hlink": theme.hlink,
        "folHlink": theme.fol_hlink,
    })
}

fn parse_drawing_pics(
    dxml: &str,
    rels: &std::collections::HashMap<String, String>,
    dpath: &str,
    files: &std::collections::HashMap<String, Vec<u8>>,
    sheet: u32,
    model: &UserModel,
) -> Vec<Value> {
    let mut objs = Vec::new();
    for (index, (start, end, anchor_kind)) in drawing_anchor_slices(dxml).into_iter().enumerate() {
        let block = &dxml[start..end];
        let geometry @ (abs_x, abs_y, width, height, row, col) =
            drawing_geometry(block, sheet, model);
        let kind = drawing_native_kind(block);
        let mut native =
            drawing_native_descriptor(dpath, index, &anchor_kind, kind, block, geometry);
        if let Some(descriptor) = native.as_object_mut() {
            // The native editor keeps schemeClr as a token and resolves it only for display.
            // Supplying the active workbook palette here avoids flattening the token and avoids
            // accidentally applying tint/shade/luminance transforms twice on a later edit.
            descriptor.insert("theme".to_string(), drawing_theme_model(&model.get_theme()));
        }
        let object_id = format!("{}-{sheet}", native["token"].as_str().unwrap_or("native"));
        if block.contains("<c:chart") || block.contains("<chart ") {
            let Some(rid) = xml_local_element_attr(block, "chart", "r:id") else {
                continue;
            };
            let Some(target) = rels.get(&rid) else {
                continue;
            };
            let chart_path = resolve_rel_path(&path_dir(dpath), target);
            let Some(bytes) = files.get(&chart_path) else {
                continue;
            };
            let chart_xml = String::from_utf8_lossy(bytes).to_string();
            let Some((code, chart_type, title)) =
                chart_xml_to_html(&chart_xml, &model.get_theme(), Some((model, sheet)))
            else {
                continue;
            };
            if let Some(descriptor) = native.as_object_mut() {
                descriptor.insert("contentPart".to_string(), Value::String(chart_path));
                descriptor.insert(
                    "model".to_string(),
                    native_chart_edit::parse_chart_model(&chart_xml),
                );
            }
            objs.push(json!({
                "id": object_id, "sheet": sheet,
                "type": "html", "mode": "abs", "r": row + 1, "c": col + 1,
                "x": abs_x, "y": abs_y, "w": width, "h": height,
                "config": {"code":code,"source":"xlsx-chart","chartType":chart_type,"title":title,"excelChartXml":chart_xml,"nativeDrawing":native},
            }));
            continue;
        }
        let (otype, mut cfg) = if let Some(rid) = xml_attr(block, "r:embed") {
            let media = rels
                .get(&rid)
                .map(|target| resolve_rel_path(&path_dir(dpath), target))
                .and_then(|path| files.get(&path).map(|bytes| (path, bytes)));
            if let Some((media_path, bytes)) = media {
                let ext = media_path.rsplit('.').next().unwrap_or("").to_lowercase();
                if ext == "emf" || ext == "wmf" {
                    match emf2svg::emf_to_svg(bytes) {
                        Ok(svg) => ("svg", json!({ "svg": svg })),
                        Err(_) => (
                            "svg",
                            json!({ "svg": drawing_preview_svg(block, kind, rels, dpath, files, &model.get_theme()) }),
                        ),
                    }
                } else {
                    (
                        "image",
                        json!({ "src": format!("data:{};base64,{}", mime_from_path(&media_path), b64_encode(bytes)) }),
                    )
                }
            } else {
                (
                    "svg",
                    json!({ "svg": drawing_preview_svg(block, kind, rels, dpath, files, &model.get_theme()) }),
                )
            }
        } else {
            (
                "svg",
                json!({ "svg": drawing_preview_svg(block, kind, rels, dpath, files, &model.get_theme()) }),
            )
        };
        if matches!(kind, "shape" | "connector" | "group") {
            if let Some(descriptor) = native.as_object_mut() {
                descriptor.insert(
                    "model".to_string(),
                    native_shape_edit::parse_shape_model(&drawing_fragment_document_with_source(
                        block,
                        Some(dxml),
                    )),
                );
            }
        } else if kind == "smartart" {
            let data_part = drawing_any_element_attr(block, "relIds", "r:dm")
                .and_then(|rid| rels.get(&rid))
                .map(|target| resolve_rel_path(&path_dir(dpath), target));
            if let Some(data_part) = data_part {
                if let Some(bytes) = files.get(&data_part) {
                    let data_xml = String::from_utf8_lossy(bytes);
                    if let Some(descriptor) = native.as_object_mut() {
                        descriptor.insert("contentPart".to_string(), Value::String(data_part));
                        descriptor.insert(
                            "model".to_string(),
                            native_smartart_edit::parse_smartart_model(&data_xml),
                        );
                    }
                }
            }
        }
        if let Some(config) = cfg.as_object_mut() {
            config.insert("source".to_string(), Value::String(format!("xlsx-{kind}")));
            config.insert("nativeDrawing".to_string(), native);
        }
        // 计算绝对像素位置：累加列宽/行高到锚定单元格 + 偏移（Excel 中图片是绝对固定的）
        objs.push(json!({
            "id": object_id,
            "sheet": sheet, "type": otype, "mode": "abs",
            "r": row + 1, "c": col + 1,
            "x": abs_x, "y": abs_y, "w": width, "h": height,
            "config": cfg,
        }));
    }
    objs
}

/// 从 workbook relationship 实际指向的主题部件解析颜色。颜色仍以主题槽位保存在
/// IronCalc 中，渲染时再按 SpreadsheetML theme 索引映射并叠加变换；这里不能提前
/// 拍平成默认 Office RGB，也不能假设部件名固定为 `xl/theme/theme1.xml`。
fn parse_xlsx_theme(snapshot: &OpcPackageSnapshot) -> Option<Theme> {
    let theme_part = snapshot_workbook_theme_part(snapshot)?;
    let xml = std::str::from_utf8(snapshot.parts.get(&theme_part)?).ok()?;
    let doc = roxmltree::Document::parse(xml).ok()?;
    let scheme = doc.descendants().find(|n| n.has_tag_name("clrScheme"))?;
    let fallback = Theme::default();
    let extract = |tag: &str, default: &str| -> String {
        let Some(slot) = scheme.children().find(|n| n.has_tag_name(tag)) else {
            return default.to_string();
        };
        let Some(color) = slot.children().find(|n| n.is_element()) else {
            return default.to_string();
        };
        // sysClr 的 val 是 window/windowText 等系统名，lastClr 才是保存文件时
        // Excel 实际使用的 RGB；srgbClr 则直接读取 val。命名空间前缀可任意。
        let raw = if color.has_tag_name("sysClr") {
            color
                .attribute("lastClr")
                .or_else(|| color.attribute("val"))
        } else if color.has_tag_name("srgbClr") {
            color.attribute("val")
        } else {
            None
        };
        match raw {
            Some(v) if v.len() == 6 && v.chars().all(|c| c.is_ascii_hexdigit()) => {
                format!("#{}", v.to_ascii_uppercase())
            }
            _ => default.to_string(),
        }
    };
    Some(Theme {
        name: scheme
            .attribute("name")
            .unwrap_or("Imported Office Theme")
            .to_string(),
        dk1: extract("dk1", &fallback.dk1),
        lt1: extract("lt1", &fallback.lt1),
        dk2: extract("dk2", &fallback.dk2),
        lt2: extract("lt2", &fallback.lt2),
        accent1: extract("accent1", &fallback.accent1),
        accent2: extract("accent2", &fallback.accent2),
        accent3: extract("accent3", &fallback.accent3),
        accent4: extract("accent4", &fallback.accent4),
        accent5: extract("accent5", &fallback.accent5),
        accent6: extract("accent6", &fallback.accent6),
        hlink: extract("hlink", &fallback.hlink),
        fol_hlink: extract("folHlink", &fallback.fol_hlink),
    })
}

fn rich_bool_property(rpr: roxmltree::Node<'_, '_>, name: &str) -> bool {
    rpr.children()
        .find(|n| n.is_element() && n.tag_name().name() == name)
        .map(|n| !matches!(n.attribute("val"), Some("0" | "false" | "off" | "none")))
        .unwrap_or(false)
}

fn rich_xml_escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn rich_xml_escape_attr(value: &str) -> String {
    rich_xml_escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn rich_text_element(value: &str) -> String {
    let preserve = value.chars().next().is_some_and(char::is_whitespace)
        || value.chars().next_back().is_some_and(char::is_whitespace);
    if preserve {
        format!(
            "<t xml:space=\"preserve\">{}</t>",
            rich_xml_escape_text(value)
        )
    } else {
        format!("<t>{}</t>", rich_xml_escape_text(value))
    }
}

fn rich_size_string(value: f64) -> String {
    let mut value = format!("{value:.6}");
    while value.contains('.') && value.ends_with('0') {
        value.pop();
    }
    if value.ends_with('.') {
        value.pop();
    }
    value
}

fn rich_color_element(color: &Color) -> Option<String> {
    match color {
        Color::Rgb(value) => Some(format!(
            "<color rgb=\"FF{}\"/>",
            rich_xml_escape_attr(value.trim_start_matches('#'))
        )),
        Color::Theme(index, tint) if *tint == 0.0 => Some(format!("<color theme=\"{index}\"/>")),
        Color::Theme(index, tint) => Some(format!(
            "<color theme=\"{index}\" tint=\"{}\"/>",
            rich_size_string(*tint)
        )),
        Color::None => None,
    }
}

fn rich_element_start_tag<'a>(xml: &'a str, node: roxmltree::Node<'_, '_>) -> Option<&'a str> {
    let range = node.range();
    let relative_end = xml.get(range.start..range.end)?.find('>')?;
    xml.get(range.start..=range.start + relative_end)
}

fn replace_direct_rpr_child(
    xml: &str,
    name: &str,
    replacement: Option<&str>,
) -> Result<String, String> {
    let document =
        roxmltree::Document::parse(xml).map_err(|error| format!("rich-text rPr XML: {error}"))?;
    let root = document.root_element();
    let existing = root
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == name)
        .map(|node| node.range());
    drop(document);
    let mut result = xml.to_string();
    if let Some(range) = existing {
        if let Some(replacement) = replacement {
            result.replace_range(range, replacement);
        } else {
            result.replace_range(range, "");
        }
        return Ok(result);
    }
    let Some(replacement) = replacement else {
        return Ok(result);
    };
    let document = roxmltree::Document::parse(&result)
        .map_err(|error| format!("rich-text rPr XML: {error}"))?;
    let root = document.root_element();
    let range = root.range();
    let start_tag = rich_element_start_tag(&result, root)
        .ok_or("rich-text rPr start tag is malformed")?
        .to_string();
    drop(document);
    if start_tag.ends_with("/>") {
        let mut open = start_tag;
        open.truncate(open.len() - 2);
        open.push('>');
        result.replace_range(range, &format!("{open}{replacement}</rPr>"));
    } else {
        let close = result
            .get(range.clone())
            .and_then(|fragment| fragment.rfind("</"))
            .map(|offset| range.start + offset)
            .ok_or("rich-text rPr closing tag is malformed")?;
        result.insert_str(close, replacement);
    }
    Ok(result)
}

fn build_rich_rpr_xml(
    template: Option<&str>,
    old: Option<&RichTextRun>,
    new: &RichTextRun,
) -> Result<Option<String>, String> {
    let has_new_properties = new.bold
        || new.italic
        || new.underline
        || new.strike
        || new.size.is_some()
        || new.font.is_some()
        || !matches!(new.color, Color::None);
    if template.is_none() && !has_new_properties {
        return Ok(None);
    }
    let mut result = template.unwrap_or("<rPr></rPr>").to_string();
    let changed = |predicate: bool| old.is_none() || predicate;
    if changed(old.is_some_and(|old| old.bold != new.bold)) {
        result = replace_direct_rpr_child(&result, "b", new.bold.then_some("<b/>"))?;
    }
    if changed(old.is_some_and(|old| old.italic != new.italic)) {
        result = replace_direct_rpr_child(&result, "i", new.italic.then_some("<i/>"))?;
    }
    if changed(old.is_some_and(|old| old.underline != new.underline)) {
        result = replace_direct_rpr_child(&result, "u", new.underline.then_some("<u/>"))?;
    }
    if changed(old.is_some_and(|old| old.strike != new.strike)) {
        result = replace_direct_rpr_child(&result, "strike", new.strike.then_some("<strike/>"))?;
    }
    if changed(old.is_some_and(|old| old.size != new.size)) {
        let replacement = new
            .size
            .map(|size| format!("<sz val=\"{}\"/>", rich_size_string(size)));
        result = replace_direct_rpr_child(&result, "sz", replacement.as_deref())?;
    }
    if changed(old.is_some_and(|old| old.font != new.font)) {
        let replacement = new
            .font
            .as_ref()
            .map(|font| format!("<rFont val=\"{}\"/>", rich_xml_escape_attr(font)));
        result = replace_direct_rpr_child(&result, "rFont", replacement.as_deref())?;
        // Some producers use the equivalent legacy <name> child.
        if template.is_some_and(|template| template.contains("<name")) {
            let name_replacement = replacement
                .as_ref()
                .map(|xml| xml.replacen("<rFont", "<name", 1));
            result = replace_direct_rpr_child(&result, "name", name_replacement.as_deref())?;
            result = replace_direct_rpr_child(&result, "rFont", None)?;
        }
    }
    if changed(old.is_some_and(|old| old.color != new.color)) {
        let replacement = rich_color_element(&new.color);
        result = replace_direct_rpr_child(&result, "color", replacement.as_deref())?;
    }
    Ok(Some(result))
}

fn patch_rich_run_xml(
    template: Option<&str>,
    old: Option<&RichTextRun>,
    new: &RichTextRun,
) -> Result<String, String> {
    if template.is_some() && old == Some(new) {
        return Ok(template.expect("checked template").to_string());
    }
    let template_rpr = template.and_then(|template| {
        let document = roxmltree::Document::parse(template).ok()?;
        let run = document.root_element();
        let rpr = run
            .children()
            .find(|node| node.is_element() && node.has_tag_name("rPr"))?;
        template.get(rpr.range()).map(str::to_string)
    });
    let rpr = build_rich_rpr_xml(template_rpr.as_deref(), old, new)?;
    let text = rich_text_element(&new.text);
    let Some(template) = template else {
        return Ok(format!("<r>{}{text}</r>", rpr.unwrap_or_default()));
    };
    let document = roxmltree::Document::parse(template)
        .map_err(|error| format!("rich-text run XML: {error}"))?;
    let run = document.root_element();
    let start_tag = rich_element_start_tag(template, run)
        .ok_or("rich-text run start tag is malformed")?
        .to_string();
    let mut output = String::new();
    output.push_str(&start_tag);
    let mut wrote_rpr = false;
    let mut wrote_text = false;
    if !run
        .children()
        .any(|node| node.is_element() && node.has_tag_name("rPr"))
    {
        if let Some(rpr) = &rpr {
            output.push_str(rpr);
            wrote_rpr = true;
        }
    }
    for child in run.children().filter(roxmltree::Node::is_element) {
        match child.tag_name().name() {
            "rPr" => {
                if let Some(rpr) = &rpr {
                    output.push_str(rpr);
                }
                wrote_rpr = true;
            }
            "t" => {
                output.push_str(&text);
                wrote_text = true;
            }
            _ => {
                if let Some(fragment) = template.get(child.range()) {
                    output.push_str(fragment);
                }
            }
        }
    }
    if !wrote_rpr {
        if let Some(rpr) = &rpr {
            output.push_str(rpr);
        }
    }
    if !wrote_text {
        output.push_str(&text);
    }
    output.push_str("</r>");
    Ok(output)
}

fn inline_string_to_shared_item(raw: &str) -> String {
    let Some(start) = raw.find("<is") else {
        return raw.to_string();
    };
    let mut result = raw.to_string();
    result.replace_range(start + 1..start + 3, "si");
    if let Some(close) = result.rfind("</is>") {
        result.replace_range(close + 2..close + 4, "si");
    }
    result
}

fn build_rich_shared_item_xml(
    template: Option<&str>,
    old_runs: &[RichTextRun],
    new_runs: &[RichTextRun],
) -> Result<String, String> {
    if template.is_some() && old_runs == new_runs {
        return Ok(template.expect("checked template").to_string());
    }
    let Some(template) = template else {
        let runs = new_runs
            .iter()
            .map(|run| patch_rich_run_xml(None, None, run))
            .collect::<Result<String, String>>()?;
        return Ok(format!("<si>{runs}</si>"));
    };
    let normalized_template = inline_string_to_shared_item(template);
    let document = roxmltree::Document::parse(&normalized_template)
        .map_err(|error| format!("rich-text shared item XML: {error}"))?;
    let item = document.root_element();
    let start_tag = rich_element_start_tag(&normalized_template, item)
        .ok_or("rich-text shared item start tag is malformed")?
        .to_string();
    let templates = item
        .children()
        .filter(|node| {
            node.is_element()
                && node.has_tag_name("r")
                && node
                    .children()
                    .filter(|child| child.is_element() && child.has_tag_name("t"))
                    .filter_map(|child| child.text())
                    .any(|text| !text.is_empty())
        })
        .filter_map(|node| normalized_template.get(node.range()).map(str::to_string))
        .collect::<Vec<_>>();
    let opaque_children = item
        .children()
        .filter(|node| {
            if !node.is_element() || node.has_tag_name("t") {
                return false;
            }
            if !node.has_tag_name("r") {
                return true;
            }
            !node
                .children()
                .filter(|child| child.is_element() && child.has_tag_name("t"))
                .filter_map(|child| child.text())
                .any(|text| !text.is_empty())
        })
        .filter_map(|node| normalized_template.get(node.range()).map(str::to_string))
        .collect::<Vec<_>>();
    drop(document);

    let new_texts = new_runs
        .iter()
        .map(|run| run.text.clone())
        .collect::<Vec<_>>();
    let source_map = rich_run_source_map(old_runs, &new_texts);

    let mut output = start_tag;
    if output.ends_with("/>") {
        output.truncate(output.len() - 2);
        output.push('>');
    }
    for (index, run) in new_runs.iter().enumerate() {
        let source = source_map[index];
        output.push_str(&patch_rich_run_xml(
            source
                .and_then(|source| templates.get(source))
                .map(String::as_str),
            source.and_then(|source| old_runs.get(source)),
            run,
        )?);
    }
    for child in opaque_children {
        output.push_str(&child);
    }
    output.push_str("</si>");
    Ok(output)
}

fn parse_rich_runs(container: roxmltree::Node<'_, '_>, _theme: &Theme) -> Vec<RichTextRun> {
    let mut runs = Vec::new();
    for run in container
        .children()
        .filter(|n| n.is_element() && n.has_tag_name("r"))
    {
        let text = run
            .children()
            .filter(|n| n.is_element() && n.has_tag_name("t"))
            .filter_map(|n| n.text())
            .collect::<String>();
        if text.is_empty() {
            continue;
        }
        let Some(rpr) = run
            .children()
            .find(|n| n.is_element() && n.has_tag_name("rPr"))
        else {
            runs.push(RichTextRun {
                text,
                ..Default::default()
            });
            continue;
        };
        let color = rpr
            .children()
            .find(|n| n.is_element() && n.has_tag_name("color"))
            .and_then(|node| {
                if let Some(raw) = node.attribute("rgb") {
                    let rgb = if raw.len() == 8 { &raw[2..] } else { raw };
                    if rgb.len() == 6 && rgb.chars().all(|c| c.is_ascii_hexdigit()) {
                        return Some(Color::Rgb(format!("#{}", rgb.to_ascii_uppercase())));
                    }
                }
                if let Some(index) = node.attribute("theme").and_then(|v| v.parse::<i32>().ok()) {
                    let tint = node
                        .attribute("tint")
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    return Some(Color::Theme(index, tint));
                }
                None
            })
            .unwrap_or(Color::None);
        let font = rpr
            .children()
            .find(|n| n.is_element() && (n.has_tag_name("rFont") || n.has_tag_name("name")))
            .and_then(|n| n.attribute("val"))
            .map(str::to_string);
        let size = rpr
            .children()
            .find(|n| n.is_element() && n.has_tag_name("sz"))
            .and_then(|n| n.attribute("val"))
            .and_then(|v| v.parse::<f64>().ok());
        runs.push(RichTextRun {
            text,
            bold: rich_bool_property(rpr, "b"),
            italic: rich_bool_property(rpr, "i"),
            underline: rich_bool_property(rpr, "u"),
            strike: rich_bool_property(rpr, "strike"),
            size,
            font,
            color,
        });
    }
    runs
}

/// Retains Excel shared-string and inline-string formatting runs. IronCalc deliberately uses
/// the flattened string as the calculation value; this side table preserves the OOXML display
/// information without changing formula semantics.
fn parse_xlsx_rich_text(
    xlsx: &[u8],
    theme: &Theme,
) -> (
    std::collections::HashMap<(u32, i32, i32), Vec<RichTextRun>>,
    std::collections::HashMap<(u32, i32, i32), String>,
) {
    use std::io::Cursor;
    let mut result = std::collections::HashMap::new();
    let mut result_xml = std::collections::HashMap::new();
    let Ok(mut za) = zip::read::ZipArchive::new(Cursor::new(xlsx.to_vec())) else {
        return (result, result_xml);
    };
    let read_xml = |za: &mut zip::read::ZipArchive<Cursor<Vec<u8>>>, name: &str| {
        let mut text = String::new();
        za.by_name(name).ok()?.read_to_string(&mut text).ok()?;
        Some(text)
    };

    let shared_xml = read_xml(&mut za, "xl/sharedStrings.xml");
    let shared_items: Vec<(Vec<RichTextRun>, String, bool)> = shared_xml
        .as_deref()
        .and_then(|xml| roxmltree::Document::parse(xml).ok().map(|doc| (xml, doc)))
        .map(|(xml, document)| {
            document
                .descendants()
                .filter(|node| node.is_element() && node.has_tag_name("si"))
                .map(|item| {
                    let raw = xml.get(item.range()).unwrap_or("").to_string();
                    let needs_exact_transport = item.attributes().len() != 0
                        || item
                            .children()
                            .any(|child| child.is_element() && child.tag_name().name() != "t");
                    (parse_rich_runs(item, theme), raw, needs_exact_transport)
                })
                .collect()
        })
        .unwrap_or_default();

    let Some(workbook_xml) = read_xml(&mut za, "xl/workbook.xml") else {
        return (result, result_xml);
    };
    let Some(rels_xml) = read_xml(&mut za, "xl/_rels/workbook.xml.rels") else {
        return (result, result_xml);
    };
    let Ok(workbook_doc) = roxmltree::Document::parse(&workbook_xml) else {
        return (result, result_xml);
    };
    let Ok(rels_doc) = roxmltree::Document::parse(&rels_xml) else {
        return (result, result_xml);
    };
    let rel_targets: std::collections::HashMap<String, String> = rels_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("Relationship"))
        .filter_map(|n| {
            Some((
                n.attribute("Id")?.to_string(),
                n.attribute("Target")?.to_string(),
            ))
        })
        .collect();

    for (sheet_index, sheet_node) in workbook_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("sheet"))
        .enumerate()
    {
        let Some(rel_id) = sheet_node
            .attributes()
            .find(|a| a.name() == "id")
            .map(|a| a.value())
        else {
            continue;
        };
        let Some(target) = rel_targets.get(rel_id) else {
            continue;
        };
        let normalized = target.replace('\\', "/");
        let sheet_path = if normalized.starts_with('/') {
            normalized.trim_start_matches('/').to_string()
        } else if normalized.starts_with("xl/") {
            normalized
        } else {
            format!("xl/{normalized}")
        };
        let Some(sheet_xml) = read_xml(&mut za, &sheet_path) else {
            continue;
        };
        let Ok(sheet_doc) = roxmltree::Document::parse(&sheet_xml) else {
            continue;
        };
        for cell in sheet_doc
            .descendants()
            .filter(|n| n.is_element() && n.has_tag_name("c"))
        {
            let Some((row, col)) = cell.attribute("r").and_then(parse_a1) else {
                continue;
            };
            let (runs, raw_shared) = match cell.attribute("t") {
                Some("s") => {
                    let index = cell
                        .children()
                        .find(|n| n.is_element() && n.has_tag_name("v"))
                        .and_then(|n| n.text())
                        .and_then(|v| v.parse::<usize>().ok());
                    if let Some((runs, raw, needs_exact_transport)) =
                        index.and_then(|i| shared_items.get(i))
                    {
                        (runs.clone(), needs_exact_transport.then(|| raw.clone()))
                    } else {
                        (Vec::new(), None)
                    }
                }
                Some("inlineStr") => cell
                    .children()
                    .find(|n| n.is_element() && n.has_tag_name("is"))
                    .map(|item| {
                        let runs = parse_rich_runs(item, theme);
                        let needs_exact_transport = item.attributes().len() != 0
                            || item
                                .children()
                                .any(|child| child.is_element() && child.tag_name().name() != "t");
                        let raw = needs_exact_transport
                            .then(|| sheet_xml.get(item.range()).unwrap_or("").to_string())
                            .map(|raw| inline_string_to_shared_item(&raw));
                        (runs, raw)
                    })
                    .unwrap_or_default(),
                _ => (Vec::new(), None),
            };
            let key = (sheet_index as u32, row, col);
            if !runs.is_empty() {
                result.insert(key, runs);
            }
            if let Some(raw) = raw_shared.filter(|raw| !raw.is_empty()) {
                result_xml.insert(key, raw);
            }
        }
    }
    (result, result_xml)
}

fn parse_formula_transport(
    snapshot: &OpcPackageSnapshot,
    model: &UserModel,
) -> FormulaTransportSnapshot {
    #[derive(Clone)]
    struct RawFormulaCell {
        reference: String,
        row: i32,
        column: i32,
        raw_formula: String,
        formula_type: String,
        shared_index: Option<String>,
        array_ref: Option<String>,
        cell_metadata: Option<String>,
        value_metadata: Option<String>,
    }

    fn range_bounds(reference: &str) -> Option<(i32, i32, i32, i32)> {
        let (start, end) = reference.split_once(':').unwrap_or((reference, reference));
        let (r0, c0) = parse_a1(start)?;
        let (r1, c1) = parse_a1(end)?;
        Some((r0.min(r1), c0.min(c1), r0.max(r1), c0.max(c1)))
    }

    let mut result = FormulaTransportSnapshot::default();
    let Some(workbook_bytes) = snapshot.parts.get("xl/workbook.xml") else {
        return result;
    };
    let Some(rels_bytes) = snapshot.parts.get("xl/_rels/workbook.xml.rels") else {
        return result;
    };
    let workbook_xml = String::from_utf8_lossy(workbook_bytes);
    let rels_xml = String::from_utf8_lossy(rels_bytes);
    let Ok(workbook_doc) = roxmltree::Document::parse(&workbook_xml) else {
        return result;
    };
    let Ok(rels_doc) = roxmltree::Document::parse(&rels_xml) else {
        return result;
    };
    let rel_targets: std::collections::HashMap<String, String> = rels_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("Relationship"))
        .filter_map(|n| {
            Some((
                n.attribute("Id")?.to_string(),
                n.attribute("Target")?.to_string(),
            ))
        })
        .collect();

    for (sheet_index, sheet) in workbook_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("sheet"))
        .enumerate()
    {
        let Some(rel_id) = sheet
            .attributes()
            .find(|a| a.name() == "id")
            .map(|a| a.value())
        else {
            continue;
        };
        let Some(target) = rel_targets.get(rel_id) else {
            continue;
        };
        let normalized = target.replace('\\', "/");
        let sheet_path = if normalized.starts_with('/') {
            normalized.trim_start_matches('/').to_string()
        } else if normalized.starts_with("xl/") {
            normalized
        } else {
            format!("xl/{normalized}")
        };
        let Some(sheet_bytes) = snapshot.parts.get(&sheet_path) else {
            continue;
        };
        let sheet_xml = String::from_utf8_lossy(sheet_bytes);
        let Ok(sheet_doc) = roxmltree::Document::parse(&sheet_xml) else {
            continue;
        };
        let mut raw_cells = Vec::new();
        for cell in sheet_doc
            .descendants()
            .filter(|n| n.is_element() && n.has_tag_name("c"))
        {
            let Some(reference) = cell.attribute("r") else {
                continue;
            };
            let Some((row, column)) = parse_a1(reference) else {
                continue;
            };
            let Some(formula) = cell
                .children()
                .find(|n| n.is_element() && n.has_tag_name("f"))
            else {
                continue;
            };
            raw_cells.push(RawFormulaCell {
                reference: reference.to_string(),
                row,
                column,
                raw_formula: sheet_xml[formula.range()].to_string(),
                formula_type: formula.attribute("t").unwrap_or("normal").to_string(),
                shared_index: formula.attribute("si").map(str::to_string),
                array_ref: formula.attribute("ref").map(str::to_string),
                cell_metadata: cell.attribute("cm").map(str::to_string),
                value_metadata: cell.attribute("vm").map(str::to_string),
            });
        }
        let array_ranges: Vec<(String, (i32, i32, i32, i32))> = raw_cells
            .iter()
            .filter(|cell| cell.formula_type == "array")
            .filter_map(|cell| {
                Some((
                    cell.reference.clone(),
                    range_bounds(cell.array_ref.as_deref()?)?,
                ))
            })
            .collect();
        let mut groups: std::collections::BTreeMap<String, FormulaTransportGroup> =
            std::collections::BTreeMap::new();
        for raw in raw_cells {
            let key = if raw.formula_type == "shared" {
                raw.shared_index.as_ref().map(|si| format!("shared:{si}"))
            } else {
                array_ranges
                    .iter()
                    .find(|(_, (r0, c0, r1, c1))| {
                        raw.row >= *r0 && raw.row <= *r1 && raw.column >= *c0 && raw.column <= *c1
                    })
                    .map(|(anchor, _)| format!("array:{anchor}"))
            };
            let Some(key) = key else {
                continue;
            };
            let baseline_content = model
                .get_cell_content(sheet_index as u32, raw.row, raw.column)
                .unwrap_or_default();
            groups
                .entry(key)
                .or_default()
                .cells
                .push(FormulaTransportCell {
                    reference: raw.reference,
                    baseline_content,
                    raw_formula: raw.raw_formula,
                    cell_metadata: raw.cell_metadata,
                    value_metadata: raw.value_metadata,
                });
        }
        if !groups.is_empty() {
            result.sheets.insert(
                sheet_path,
                FormulaTransportSheet {
                    sheet_index: sheet_index as u32,
                    groups: groups.into_values().collect(),
                },
            );
        }
    }
    result
}

fn inject_drawings(
    xlsx: Vec<u8>,
    objects: &[Value],
    equations: &[Value],
) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Read, Write};
    // 按 sheet 分组对象与 LaTeX 公式
    let mut by_sheet: std::collections::HashMap<u32, Vec<&Value>> =
        std::collections::HashMap::new();
    for o in objects {
        let sheet = o["sheet"].as_u64().unwrap_or(0) as u32;
        by_sheet.entry(sheet).or_default().push(o);
    }
    let mut eq_by_sheet: std::collections::HashMap<u32, Vec<&Value>> =
        std::collections::HashMap::new();
    for e in equations {
        let sheet = e["sheet"].as_u64().unwrap_or(0) as u32;
        eq_by_sheet.entry(sheet).or_default().push(e);
    }
    // 公式单元格引用（导出时清除单元格里的原始 $..$ 文本）
    let mut eq_cells: std::collections::HashMap<u32, Vec<String>> =
        std::collections::HashMap::new();
    for e in equations {
        let sheet = e["sheet"].as_u64().unwrap_or(0) as u32;
        let r = e["r"].as_i64().unwrap_or(1) as i32;
        let c = e["c"].as_i64().unwrap_or(1) as i32;
        eq_cells.entry(sheet).or_default().push(cell_ref(r, c));
    }
    if by_sheet.is_empty() && eq_by_sheet.is_empty() {
        return Ok(xlsx);
    }
    // 所有涉及的 sheet（对象 ∪ 公式）
    let mut all_sheets: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    all_sheets.extend(by_sheet.keys().copied());
    all_sheets.extend(eq_by_sheet.keys().copied());
    let mut za =
        zip::read::ZipArchive::new(Cursor::new(xlsx)).map_err(|e| format!("zip read: {e}"))?;
    let mut zw = zip::write::ZipWriter::new(Cursor::new(Vec::new()));
    let opts =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    // 预生成每个对象 sheet 的 drawing xml/rels 与 media
    let mut media_files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut drawing_files: Vec<(String, String)> = Vec::new(); // (路径, 内容)
    let mut drawing_for_sheet: std::collections::HashMap<u32, (usize, usize)> =
        std::collections::HashMap::new(); // sheet -> (drawing序号, media起始)
    let mut media_idx = 0usize;
    let mut drawing_idx = 0usize;
    for sheet in &all_sheets {
        let empty_objs: Vec<&Value> = Vec::new();
        let objs = by_sheet.get(sheet).unwrap_or(&empty_objs);
        drawing_idx += 1;
        let mut pics = String::new();
        let mut rels = String::new();
        let mut pic_id = 1usize;
        for o in objs {
            media_idx += 1;
            let Some((media_name, media_bytes, _is_png)) = object_media(o, media_idx) else {
                continue;
            };
            pic_id += 1;
            let rid = format!("rId{pic_id}");
            let (r, c, x, y, w, h) = (
                o["r"].as_i64().unwrap_or(1),
                o["c"].as_i64().unwrap_or(1),
                o["x"].as_i64().unwrap_or(0),
                o["y"].as_i64().unwrap_or(0),
                o["w"].as_i64().unwrap_or(100),
                o["h"].as_i64().unwrap_or(100),
            );
            let mode = o["mode"].as_str().unwrap_or("cell");
            let (cx, cy) = (w * EMU_PER_PX, h * EMU_PER_PX);
            let anchor = if mode == "abs" {
                format!(
                    "<xdr:absoluteAnchor><xdr:pos x=\"{}\" y=\"{}\"/><xdr:ext cx=\"{}\" cy=\"{}\"/>",
                    x * EMU_PER_PX,
                    y * EMU_PER_PX,
                    cx,
                    cy
                )
            } else {
                format!(
                    "<xdr:oneCellAnchor><xdr:from><xdr:col>{}</xdr:col><xdr:colOff>{}</xdr:colOff><xdr:row>{}</xdr:row><xdr:rowOff>{}</xdr:rowOff></xdr:from><xdr:ext cx=\"{}\" cy=\"{}\"/>",
                    c - 1,
                    x * EMU_PER_PX,
                    r - 1,
                    y * EMU_PER_PX,
                    cx,
                    cy
                )
            };
            let oid = o["id"].as_str().unwrap_or("obj");
            let close = if mode == "abs" {
                "</xdr:absoluteAnchor>"
            } else {
                "</xdr:oneCellAnchor>"
            };
            pics.push_str(&format!(
                "{anchor}<xdr:pic><xdr:nvPicPr><xdr:cNvPr id=\"{pic_id}\" name=\"{oid}\"/><xdr:cNvPicPr/></xdr:nvPicPr><xdr:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></xdr:spPr></xdr:pic><xdr:clientData/>{close}"
            ));
            rels.push_str(&format!("<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"../media/{media_name}\"/>"));
            media_files.push((format!("xl/media/{media_name}"), media_bytes));
        }
        // 追加 LaTeX 公式的 OMML 形状（与图片同处一个 sheet drawing；白底盖住底层 $..$ 文本）
        for eq in eq_by_sheet.get(sheet).unwrap_or(&empty_objs) {
            pic_id += 1;
            let (r, c, w, h) = (
                eq["r"].as_i64().unwrap_or(1),
                eq["c"].as_i64().unwrap_or(1),
                eq["w"].as_i64().unwrap_or(200),
                eq["h"].as_i64().unwrap_or(40),
            );
            let (cx, cy) = (w * EMU_PER_PX, h * EMU_PER_PX);
            let omml = eq["omml"].as_str().unwrap_or("");
            let fb = html_escape(eq["fallback"].as_str().unwrap_or(""));
            pics.push_str(&format!(
                "<xdr:oneCellAnchor><xdr:from><xdr:col>{col}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{row}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:ext cx=\"{cx}\" cy=\"{cy}\"/><xdr:sp macro=\"\" textlink=\"\"><xdr:nvSpPr><xdr:cNvPr id=\"{pic_id}\" name=\"Eq{pic_id}\"/><xdr:cNvSpPr txBox=\"1\"/></xdr:nvSpPr><xdr:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val=\"FFFFFF\"/></a:solidFill><a:ln><a:noFill/></a:ln></xdr:spPr><xdr:txBody><a:bodyPr wrap=\"none\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\" rtlCol=\"0\"/><a:lstStyle/><a:p><a:pPr algn=\"l\"/><mc:AlternateContent><mc:Choice Requires=\"a14\"><a14:m>{omml}</a14:m></mc:Choice><mc:Fallback><a:r><a:rPr lang=\"en-US\"/><a:t>{fb}</a:t></a:r></mc:Fallback></mc:AlternateContent><a:endParaRPr lang=\"en-US\"/></a:p></xdr:txBody></xdr:sp><xdr:clientData/></xdr:oneCellAnchor>",
                col = c - 1, row = r - 1
            ));
        }
        let drawing_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" xmlns:a14=\"http://schemas.microsoft.com/office/drawing/2010/main\" xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\" mc:Ignorable=\"a14\">{pics}</xdr:wsDr>"
        );
        drawing_files.push((
            format!("xl/drawings/unicellDrawing{drawing_idx}.xml"),
            drawing_xml,
        ));
        drawing_files.push((
            format!("xl/drawings/_rels/unicellDrawing{drawing_idx}.xml.rels"),
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>"),
        ));
        drawing_for_sheet.insert(*sheet, (drawing_idx, 0));
    }
    // A worksheet may already own hyperlinks, comments, tables or other relationships. Allocate a
    // genuinely free id and append our drawing relation instead of assuming `rIdN` is available.
    let mut drawing_rel_for_path: std::collections::HashMap<String, (usize, String)> =
        std::collections::HashMap::new();
    for (sheet_idx, (drawing, _)) in &drawing_for_sheet {
        let rels_path = format!("xl/worksheets/_rels/sheet{}.xml.rels", sheet_idx + 1);
        let mut used = std::collections::HashSet::new();
        if let Ok(mut rels_file) = za.by_name(&rels_path) {
            let mut rels_xml = String::new();
            let _ = rels_file.read_to_string(&mut rels_xml);
            if let Ok(doc) = roxmltree::Document::parse(&rels_xml) {
                used.extend(
                    doc.descendants()
                        .filter(|n| n.is_element() && n.has_tag_name("Relationship"))
                        .filter_map(|n| n.attribute("Id").map(str::to_string)),
                );
            }
        }
        let mut suffix = *drawing;
        let rid = loop {
            let candidate = format!("rIdDrawing{suffix}");
            if !used.contains(&candidate) {
                break candidate;
            }
            suffix += 1;
        };
        drawing_rel_for_path.insert(rels_path, (*drawing, rid));
    }
    // 复制原 zip 条目，补丁 content_types 与 sheet 引用
    let mut existing_sheet_rels = std::collections::HashSet::new();
    for i in 0..za.len() {
        let mut f = za.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        let name = f.name().to_string();
        let mut data = Vec::new();
        f.read_to_end(&mut data)
            .map_err(|e| format!("zip read entry: {e}"))?;
        if name == "[Content_Types].xml" {
            let mut txt = String::from_utf8_lossy(&data).to_string();
            if !txt.contains("Extension=\"png\"") {
                txt = txt.replace(
                    "</Types>",
                    "<Default Extension=\"png\" ContentType=\"image/png\"/></Types>",
                );
            }
            if !txt.contains("Extension=\"emf\"") {
                txt = txt.replace(
                    "</Types>",
                    "<Default Extension=\"emf\" ContentType=\"image/x-emf\"/></Types>",
                );
            }
            for d in 1..=drawing_idx {
                txt = txt.replace("</Types>", &format!("<Override PartName=\"/xl/drawings/unicellDrawing{d}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.drawing+xml\"/></Types>"));
            }
            data = txt.into_bytes();
        } else if let Some(rest) = name.strip_prefix("xl/worksheets/sheet") {
            if let Some(num_str) = rest.strip_suffix(".xml") {
                if let Ok(sheet_num) = num_str.parse::<u32>() {
                    let sheet_idx = sheet_num - 1;
                    if drawing_for_sheet.contains_key(&sheet_idx) {
                        let rels_path = format!("xl/worksheets/_rels/sheet{sheet_num}.xml.rels");
                        let rid = drawing_rel_for_path
                            .get(&rels_path)
                            .map(|(_, rid)| rid.as_str())
                            .unwrap_or("rIdDrawing1");
                        let mut txt = String::from_utf8_lossy(&data).to_string();
                        // 清除公式单元格的原始 $..$ 文本（Excel 约定：公式为浮动对象，单元格不写 LaTeX）
                        if let Some(refs) = eq_cells.get(&sheet_idx) {
                            for cref in refs {
                                txt = remove_cell_from_sheet(&txt, cref);
                            }
                        }
                        txt = txt.replace(
                            "</worksheet>",
                            &format!("<drawing r:id=\"{rid}\"/></worksheet>"),
                        );
                        data = txt.into_bytes();
                    }
                }
            }
        } else if let Some((drawing, rid)) = drawing_rel_for_path.get(&name) {
            let mut txt = String::from_utf8_lossy(&data).to_string();
            if !txt.contains(&format!("Id=\"{rid}\"")) {
                let relation = format!(
                    "<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/unicellDrawing{drawing}.xml\"/>"
                );
                txt = txt.replace("</Relationships>", &format!("{relation}</Relationships>"));
            }
            data = txt.into_bytes();
            existing_sheet_rels.insert(name.clone());
        }
        zw.start_file(&name, opts)
            .map_err(|e| format!("zip write: {e}"))?;
        zw.write_all(&data)
            .map_err(|e| format!("zip write data: {e}"))?;
    }
    // 新增 sheet rels（若原 zip 无则创建）
    for (rels_path, (d, rid)) in &drawing_rel_for_path {
        if !existing_sheet_rels.contains(rels_path) {
            let rels = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/unicellDrawing{d}.xml\"/></Relationships>"
            );
            zw.start_file(rels_path.as_str(), opts)
                .map_err(|e| format!("zip rels: {e}"))?;
            zw.write_all(rels.as_bytes())
                .map_err(|e| format!("zip rels data: {e}"))?;
        }
    }
    // 新增 drawing 与 media
    for (path, content) in &drawing_files {
        zw.start_file(path, opts)
            .map_err(|e| format!("zip drawing: {e}"))?;
        zw.write_all(content.as_bytes())
            .map_err(|e| format!("zip drawing data: {e}"))?;
    }
    for (path, bytes) in &media_files {
        zw.start_file(path, opts)
            .map_err(|e| format!("zip media: {e}"))?;
        zw.write_all(bytes)
            .map_err(|e| format!("zip media data: {e}"))?;
    }
    let res = zw.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(res.into_inner())
}

// ---------- UDOC3 容器（参考母项目 unidoc 结构：魔数 + 部件 + br 中央目录 + 64B 尾） ----------
const UDOC3_HEADER: &[u8; 8] = b"UDOC3PKG";
const UDOC3_FOOTER_MAGIC: &[u8; 8] = b"UD3DIR01";
const UDOC3_FOOTER_SIZE: usize = 64;

fn sha256_hex(data: &[u8]) -> String {
    let mut h = sha2::Sha256::new();
    h.update(data);
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

fn br_compress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut params = brotli::enc::BrotliEncoderParams::default();
    params.quality = 9;
    params.lgwin = 22;
    params.size_hint = data.len();
    params.mode = brotli::enc::backward_references::BrotliEncoderMode::BROTLI_MODE_TEXT;
    brotli::BrotliCompress(&mut &data[..], &mut out, &params)
        .map_err(|e| format!("brotli: {e}"))?;
    Ok(out)
}

fn valid_udoc3_entry_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn udoc3_entry_extension(path: &str) -> &str {
    path.rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("")
}

fn udoc3_uses_brotli(path: &str, mime: &str) -> bool {
    let mime = mime
        .split_once(';')
        .map(|(value, _)| value)
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    let extension = udoc3_entry_extension(path).to_ascii_lowercase();
    mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/xhtml+xml"
                | "image/svg+xml"
        )
        || matches!(
            extension.as_str(),
            "json"
                | "html"
                | "htm"
                | "svg"
                | "xml"
                | "css"
                | "js"
                | "mjs"
                | "md"
                | "markdown"
                | "txt"
        )
}

fn udoc3_zip_should_store(path: &str) -> bool {
    matches!(
        udoc3_entry_extension(path).to_ascii_lowercase().as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "apng"
            | "avif"
            | "mp4"
            | "webm"
            | "ogv"
            | "mov"
            | "mp3"
            | "ogg"
            | "m4a"
            | "aac"
            | "flac"
            | "weba"
            | "zip"
            | "gz"
            | "br"
            | "pdf"
            | "woff"
            | "woff2"
            | "xlsx"
            | "xlsm"
            | "xlsb"
    )
}

fn zip_pack_udoc3_entry(path: &str, data: &[u8], store: bool) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Write};

    if !valid_udoc3_entry_path(path) {
        return Err(format!("UDOC3 ZIP 部件路径无效：{path}"));
    }
    if data.len() > MAX_UDOC3_ENTRY_SIZE {
        return Err(format!("UDOC3 ZIP 部件超过安全上限：{path}"));
    }
    let mut writer = zip::write::ZipWriter::new(Cursor::new(Vec::new()));
    let method = if store {
        zip::CompressionMethod::Stored
    } else {
        zip::CompressionMethod::Deflated
    };
    let options = zip::write::FileOptions::default()
        .compression_method(method)
        .compression_level((!store).then_some(6))
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    writer
        .start_file(path, options)
        .map_err(|error| format!("UDOC3 ZIP 建档失败（{path}）：{error}"))?;
    writer
        .write_all(data)
        .map_err(|error| format!("UDOC3 ZIP 写入失败（{path}）：{error}"))?;
    let payload = writer
        .finish()
        .map_err(|error| format!("UDOC3 ZIP 收尾失败（{path}）：{error}"))?
        .into_inner();
    if payload.len() > MAX_UDOC3_ZIP_SIZE {
        return Err(format!("UDOC3 ZIP 编码后超过安全上限：{path}"));
    }
    Ok(payload)
}

fn zip_unpack_udoc3_entry(
    path: &str,
    payload: &[u8],
    expected_size: usize,
) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Read};

    if !valid_udoc3_entry_path(path) {
        return Err(format!("UDOC3 ZIP 部件路径无效：{path}"));
    }
    if payload.len() > MAX_UDOC3_ZIP_SIZE || expected_size > MAX_UDOC3_ENTRY_SIZE {
        return Err(format!("UDOC3 ZIP 部件超过安全上限：{path}"));
    }
    let mut archive = zip::read::ZipArchive::new(Cursor::new(payload))
        .map_err(|error| format!("UDOC3 ZIP 结构无效（{path}）：{error}"))?;
    if archive.len() != 1 {
        return Err(format!("UDOC3 ZIP 部件必须且只能包含一个文件：{path}"));
    }
    let mut file = archive
        .by_index(0)
        .map_err(|error| format!("UDOC3 ZIP 读取失败（{path}）：{error}"))?;
    if file.is_dir() || file.name() != path || file.size() != expected_size as u64 {
        return Err(format!("UDOC3 ZIP 路径或声明尺寸不匹配：{path}"));
    }
    if !matches!(
        file.compression(),
        zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
    ) {
        return Err(format!("UDOC3 ZIP 使用了不允许的压缩算法：{path}"));
    }
    let mut bytes = Vec::with_capacity(expected_size.min(8 * 1024 * 1024));
    file.by_ref()
        .take(expected_size as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("UDOC3 ZIP 解压失败（{path}）：{error}"))?;
    if bytes.len() != expected_size {
        return Err(format!(
            "UDOC3 ZIP 解压尺寸不匹配（{path}）：声明 {expected_size}，实际 {}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

fn parse_data_url(src: &str) -> Option<(String, Vec<u8>)> {
    let rest = src.strip_prefix("data:")?;
    let (meta, b64) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64").unwrap_or(meta).to_string();
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    Some((mime, bytes))
}

// 部件打包为 UDOC3 容器：文本/SVG 独立 Brotli q9，二进制独立单文件 ZIP。
// 已压缩格式使用 ZIP Store；BMP/WAV 等原始二进制使用 ZIP Deflate。
fn encode_udoc3(parts: Vec<(String, Vec<u8>, String)>) -> Result<Vec<u8>, String> {
    if parts.len() > MAX_UDOC3_ENTRIES {
        return Err("UDOC3 部件数量超过安全上限".into());
    }
    let mut out = UDOC3_HEADER.to_vec();
    let mut entries = Vec::new();
    let mut paths = std::collections::HashSet::new();
    let mut total_size = 0u64;
    for (path, bytes, mime) in &parts {
        if !valid_udoc3_entry_path(path) || !paths.insert(path.clone()) {
            return Err(format!("UDOC3 部件路径无效或重复：{path}"));
        }
        if bytes.len() > MAX_UDOC3_ENTRY_SIZE {
            return Err(format!("UDOC3 部件超过安全上限：{path}"));
        }
        total_size = total_size
            .checked_add(bytes.len() as u64)
            .ok_or("UDOC3 解压后总尺寸溢出")?;
        if total_size > MAX_UDOC3_TOTAL_SIZE {
            return Err("UDOC3 解压后总尺寸超过安全上限".into());
        }
        let (codec, payload) = if udoc3_uses_brotli(path, mime) && bytes.len() >= 96 {
            let comp = br_compress(bytes)?;
            if comp.len().saturating_add(16) < bytes.len() {
                ("br", comp)
            } else {
                ("store", bytes.clone())
            }
        } else if udoc3_uses_brotli(path, mime) {
            ("store", bytes.clone())
        } else {
            (
                "zip",
                zip_pack_udoc3_entry(path, bytes, udoc3_zip_should_store(path))?,
            )
        };
        let offset = out.len() as u64;
        entries.push(json!({
            "path": path, "offset": offset,
            "compressedSize": payload.len(), "size": bytes.len(),
            "codec": codec, "mime": mime, "sha256": sha256_hex(bytes),
        }));
        out.extend_from_slice(&payload);
    }
    let dir_json = serde_json::to_string(&json!({
        "format": "udoc-directory", "version": 3,
        "root": "manifest.json", "entries": entries,
    }))
    .map_err(|e| e.to_string())?;
    if dir_json.is_empty() || dir_json.len() > MAX_UDOC3_ENTRY_SIZE {
        return Err("UDOC3 尾目录尺寸超过安全上限".into());
    }
    let dir_hash = {
        let mut h = sha2::Sha256::new();
        h.update(dir_json.as_bytes());
        h.finalize()
    };
    let dir_packed = br_compress(dir_json.as_bytes())?;
    let dir_offset = out.len() as u64;
    out.extend_from_slice(&dir_packed);
    let mut footer = vec![0u8; UDOC3_FOOTER_SIZE];
    footer[0..8].copy_from_slice(UDOC3_FOOTER_MAGIC);
    footer[8..16].copy_from_slice(&dir_offset.to_le_bytes());
    footer[16..24].copy_from_slice(&(dir_packed.len() as u64).to_le_bytes());
    footer[24..56].copy_from_slice(&dir_hash);
    footer[56..60].copy_from_slice(&3u32.to_le_bytes());
    footer[60..64].copy_from_slice(&(dir_json.len() as u32).to_le_bytes());
    out.extend_from_slice(&footer);
    Ok(out)
}

// 查看.udoc：返回当前文档的 udoc 结构化 JSON（供对话框展示，不打 UDOC3 容器）
fn api_udoc_json(st: &AppState) -> Result<Resp, String> {
    let wb = &st.model.get_model().workbook;
    let names = wb.get_worksheet_names();
    let mut sheets = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let sheet = i as u32;
        let objects = st.objects.get(&sheet).cloned().unwrap_or_default();
        let merges = st.model.get_merged_cells(sheet).unwrap_or_default();
        sheets.push(json!({
            "name": name,
            "derivedView": udoc_sheet_chunk(st, sheet, name)?,
            "objects": objects,
            "merges": merges
        }));
    }
    ok_json(json!({
        "format": "udoc",
        "version": 3,
        "unidoc_type": "cell",
        "app": "UniCell",
        "manifest": {
            "format": "udoc-package",
            "version": 3,
            "basename": st.file_name,
            "sheetCount": names.len(),
            "authoritative": "document/workbook.xlsx",
            "compression": {
                "strategy": "hybrid-br-zip",
                "text": "brotli-q9",
                "binary": "single-entry-zip-store-or-deflate",
                "directory": "brotli-q9",
                "wholeFile": false
            },
            "derivedViews": {
                "digest": "document/digest.json",
                "chunks": "document/chunks/sheet{n}.json",
                "schema": "unicell-row-major-v2",
                "layouts": ["dense", "sparse"],
                "authoritative": false
            }
        },
        "digest": ai_workbook_digest(st)?,
        "sheets": sheets,
    }))
}

// SVG → EMF（集成 vecmeta 纯 Rust 库）
fn api_svg2emf(_st: &AppState, body: &[u8]) -> Result<Resp, String> {
    let v = parse_body(body)?;
    let svg = js(&v, "svg")?;
    let emf = svg2emf::svg_to_emf(svg, svg2emf::EmitOptions::default())
        .map_err(|e| format!("svg2emf: {e}"))?;
    Ok(tiny_http::Response::from_data(emf)
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/octet-stream"[..])
                .unwrap(),
        ))
}

// EMF → SVG（集成 vecmeta 纯 Rust 库）
fn api_emf2svg(_st: &AppState, body: &[u8]) -> Result<Resp, String> {
    let svg = emf2svg::emf_to_svg(body).map_err(|e| format!("emf2svg: {e}"))?;
    Ok(tiny_http::Response::from_data(svg.into_bytes())
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(
                &b"Content-Type"[..],
                &b"image/svg+xml; charset=utf-8"[..],
            )
            .unwrap(),
        ))
}

// ================= 自定义 basename + 多格式导入导出（html / excel / udoc） =================

// 从 query 的 name 参数取导出主名（percent-decoded），空则回退当前文件名
fn export_basename(st: &AppState, query: &str) -> String {
    match qget(query, "name") {
        Some(raw) => {
            let d = percent_decode(raw);
            if d.trim().is_empty() {
                st.file_name.clone()
            } else {
                d
            }
        }
        None => st.file_name.clone(),
    }
}

// 极简 percent-decode（+ 当空格；%XX 十六进制）
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let hex = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn b64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| format!("base64: {e}"))
}

fn br_decompress_exact(data: &[u8], expected_size: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;

    if expected_size > MAX_UDOC3_ENTRY_SIZE {
        return Err("UDOC3 Brotli 解压尺寸超过安全上限".into());
    }
    let decoder = brotli::Decompressor::new(data, 64 * 1024);
    let mut limited = decoder.take(expected_size as u64 + 1);
    let mut out = Vec::with_capacity(expected_size.min(8 * 1024 * 1024));
    limited
        .read_to_end(&mut out)
        .map_err(|e| format!("brotli decompress: {e}"))?;
    if out.len() != expected_size {
        return Err(format!(
            "UDOC3 Brotli 解压尺寸不匹配：声明 {expected_size}，实际 {}",
            out.len()
        ));
    }
    Ok(out)
}

fn br_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let decoder = brotli::Decompressor::new(data, 64 * 1024);
    let mut limited = decoder.take(MAX_UDOC3_ENTRY_SIZE as u64 + 1);
    let mut out = Vec::new();
    limited
        .read_to_end(&mut out)
        .map_err(|e| format!("brotli decompress: {e}"))?;
    if out.len() > MAX_UDOC3_ENTRY_SIZE {
        return Err("Brotli 解压尺寸超过安全上限".into());
    }
    Ok(out)
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Udoc3DirectoryEntry {
    path: String,
    offset: u64,
    compressed_size: u64,
    size: u64,
    codec: String,
    sha256: String,
}

#[derive(Debug, serde::Deserialize)]
struct Udoc3Directory {
    format: String,
    version: u32,
    entries: Vec<Udoc3DirectoryEntry>,
}

fn decode_udoc3_directory(data: &[u8]) -> Result<(usize, Udoc3Directory), String> {
    if data.len() < UDOC3_HEADER.len() + UDOC3_FOOTER_SIZE {
        return Err("udoc too small".into());
    }
    if &data[0..UDOC3_HEADER.len()] != UDOC3_HEADER {
        return Err("不是 UDOC3 容器（魔数不匹配）".into());
    }
    let footer = &data[data.len() - UDOC3_FOOTER_SIZE..];
    if &footer[0..8] != UDOC3_FOOTER_MAGIC {
        return Err("UDOC3 尾部魔数不匹配".into());
    }
    let dir_offset_u64 =
        u64::from_le_bytes(footer[8..16].try_into().map_err(|_| "UDOC3 目录偏移无效")?);
    let dir_comp_len_u64 = u64::from_le_bytes(
        footer[16..24]
            .try_into()
            .map_err(|_| "UDOC3 目录长度无效")?,
    );
    let version = u32::from_le_bytes(footer[56..60].try_into().map_err(|_| "UDOC3 版本无效")?);
    let dir_raw_len = u32::from_le_bytes(
        footer[60..64]
            .try_into()
            .map_err(|_| "UDOC3 目录原始长度无效")?,
    ) as usize;
    let dir_offset = usize::try_from(dir_offset_u64).map_err(|_| "UDOC3 目录偏移过大")?;
    let dir_comp_len = usize::try_from(dir_comp_len_u64).map_err(|_| "UDOC3 目录长度过大")?;
    let directory_end = dir_offset
        .checked_add(dir_comp_len)
        .ok_or("UDOC3 目录范围溢出")?;
    if version != 3
        || dir_offset < UDOC3_HEADER.len()
        || dir_comp_len == 0
        || dir_raw_len == 0
        || dir_raw_len > MAX_UDOC3_ENTRY_SIZE
        || directory_end != data.len() - UDOC3_FOOTER_SIZE
    {
        return Err("UDOC3 目录范围或版本无效".into());
    }
    let dir_json = br_decompress_exact(&data[dir_offset..directory_end], dir_raw_len)?;
    let dir_hash = sha2::Sha256::digest(&dir_json);
    if dir_hash.as_slice() != &footer[24..56] {
        return Err("UDOC3 尾目录校验失败".into());
    }
    let directory: Udoc3Directory =
        serde_json::from_slice(&dir_json).map_err(|e| format!("dir json: {e}"))?;
    if directory.format != "udoc-directory" || directory.version != 3 {
        return Err("UDOC3 目录结构无效".into());
    }
    if directory.entries.len() > MAX_UDOC3_ENTRIES {
        return Err("UDOC3 部件数量超过安全上限".into());
    }
    let mut paths = std::collections::HashSet::new();
    let mut ranges = Vec::with_capacity(directory.entries.len());
    let mut total_size = 0u64;
    for entry in &directory.entries {
        let offset = usize::try_from(entry.offset).map_err(|_| "UDOC3 部件偏移过大")?;
        let compressed_size =
            usize::try_from(entry.compressed_size).map_err(|_| "UDOC3 部件编码尺寸过大")?;
        let size = usize::try_from(entry.size).map_err(|_| "UDOC3 部件尺寸过大")?;
        let end = offset
            .checked_add(compressed_size)
            .ok_or("UDOC3 部件范围溢出")?;
        let sha_is_valid = entry.sha256.len() == 64
            && entry
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !valid_udoc3_entry_path(&entry.path)
            || !paths.insert(entry.path.clone())
            || offset < UDOC3_HEADER.len()
            || end > dir_offset
            || size > MAX_UDOC3_ENTRY_SIZE
            || !matches!(entry.codec.as_str(), "store" | "br" | "zip")
            || !sha_is_valid
            || (entry.codec == "store" && compressed_size != size)
            || (entry.codec == "zip" && compressed_size > MAX_UDOC3_ZIP_SIZE)
        {
            return Err(format!("UDOC3 目录项无效：{}", entry.path));
        }
        total_size = total_size
            .checked_add(entry.size)
            .ok_or("UDOC3 解压后总尺寸溢出")?;
        if total_size > MAX_UDOC3_TOTAL_SIZE {
            return Err("UDOC3 解压后总尺寸超过安全上限".into());
        }
        ranges.push((offset, end, entry.path.as_str()));
    }
    ranges.sort_unstable_by_key(|range| range.0);
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err(format!("UDOC3 目录项重叠：{} / {}", pair[0].2, pair[1].2));
        }
    }
    Ok((dir_offset, directory))
}

// 解析 UDOC3 容器 → path -> 解压后的部件字节。兼容旧版 store/br，
// 同时读取新混合容器的单文件 ZIP 二进制部件。
fn decode_udoc3(data: &[u8]) -> Result<std::collections::HashMap<String, Vec<u8>>, String> {
    let (_, directory) = decode_udoc3_directory(data)?;
    let mut map = std::collections::HashMap::new();
    for entry in directory.entries {
        let offset = entry.offset as usize;
        let compressed_size = entry.compressed_size as usize;
        let size = entry.size as usize;
        let payload = &data[offset..offset + compressed_size];
        let bytes = match entry.codec.as_str() {
            "br" => br_decompress_exact(payload, size)?,
            "zip" => zip_unpack_udoc3_entry(&entry.path, payload, size)?,
            "store" => payload.to_vec(),
            _ => unreachable!("codec was validated"),
        };
        if bytes.len() != size || sha256_hex(&bytes) != entry.sha256 {
            return Err(format!("UDOC3 部件校验失败：{}", entry.path));
        }
        map.insert(entry.path, bytes);
    }
    Ok(map)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// 紧凑样式 JSON → 内联 CSS（HTML 可视化表格）
fn style_css_from_json(sj: &Value) -> String {
    let mut css = String::new();
    if sj["b"].as_bool() == Some(true) {
        css.push_str("font-weight:bold;");
    }
    if sj["i"].as_bool() == Some(true) {
        css.push_str("font-style:italic;");
    }
    let mut deco = String::new();
    if sj["u"].as_bool() == Some(true) {
        deco.push_str("underline ");
    }
    if sj["st"].as_bool() == Some(true) {
        deco.push_str("line-through ");
    }
    if !deco.is_empty() {
        css.push_str(&format!("text-decoration:{};", deco.trim()));
    }
    if let Some(font) = sj["fn"].as_str() {
        if !font.is_empty() {
            css.push_str(&format!(
                "font-family:'{}';",
                font.replace('\\', "\\\\").replace('\'', "\\'")
            ));
        }
    }
    if let Some(size) = sj["sz"].as_f64() {
        if size > 0.0 {
            css.push_str(&format!("font-size:{size:.2}pt;"));
        }
    }
    if let Some(fc) = sj["fc"].as_str() {
        if !fc.is_empty() {
            css.push_str(&format!("color:{};", fc));
        }
    }
    if let Some(bg) = sj["bg"].as_str() {
        if !bg.is_empty() {
            css.push_str(&format!("background:{};", bg));
        }
    }
    if let Some(ha) = sj["ha"].as_str() {
        match ha {
            "center" | "centercontinuous" | "distributed" | "fill" | "justify" => {
                css.push_str("text-align:center;")
            }
            "right" => css.push_str("text-align:right;"),
            "left" => css.push_str("text-align:left;"),
            _ => {}
        }
    }
    if let Some(va) = sj["va"].as_str() {
        match va {
            "center" | "distributed" | "justify" => css.push_str("vertical-align:middle;"),
            "top" => css.push_str("vertical-align:top;"),
            _ => css.push_str("vertical-align:bottom;"),
        }
    }
    if sj["wr"].as_bool() == Some(true) {
        css.push_str("white-space:pre-wrap;overflow-wrap:anywhere;");
    }
    let border_css = |side: &str, item: &Value| -> String {
        if item.is_null() {
            return String::new();
        }
        let style = item["s"].as_str().unwrap_or("thin");
        let color = item["c"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or("#000");
        let (width, line) = match style {
            "hair" => ("0.5px", "solid"),
            "medium" | "mediumdashed" | "mediumdashdot" | "mediumdashdotdot" => ("2px", "solid"),
            "thick" => ("3px", "solid"),
            "double" => ("3px", "double"),
            "dotted" => ("1px", "dotted"),
            "dashed" | "dashdot" | "dashdotdot" | "slantdashdot" => ("1px", "dashed"),
            _ => ("1px", "solid"),
        };
        format!("border-{side}:{width} {line} {color};")
    };
    if let Some(br) = sj.get("br") {
        css.push_str(&border_css("top", &br["t"]));
        css.push_str(&border_css("bottom", &br["b"]));
        css.push_str(&border_css("left", &br["l"]));
        css.push_str(&border_css("right", &br["r"]));
    }
    css
}

// 从 UniCell HTML 提取内嵌 JSON（<script id=\"unicell-data\">...</script>）
fn extract_unicell_script(html: &str) -> Option<String> {
    let id = html.find("id=\"unicell-data\"")?;
    let gt = html[id..].find('>')? + id + 1;
    let end = html[gt..].find("</script>")? + gt;
    Some(html[gt..end].trim().to_string())
}

fn mime_from_path(p: &str) -> String {
    let ext = p.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
    .to_string()
}

// 把对象 config 里的 media/... 引用还原为 DataURL（image/video）或内联 SVG 文本
fn inline_object_media(oj: &mut Value, parts: &std::collections::HashMap<String, Vec<u8>>) {
    let otype = oj["type"].as_str().unwrap_or("").to_string();
    if otype == "svg" {
        if let Some(p) = oj["config"]["svg"].as_str() {
            if p.starts_with("media/") {
                if let Some(bytes) = parts.get(p) {
                    oj["config"]["svg"] = json!(String::from_utf8_lossy(bytes));
                }
            }
        }
    } else if otype == "image" || otype == "video" {
        if let Some(p) = oj["config"]["src"].as_str() {
            if p.starts_with("media/") {
                if let Some(bytes) = parts.get(p) {
                    let mime = mime_from_path(p);
                    oj["config"]["src"] =
                        json!(format!("data:{};base64,{}", mime, b64_encode(bytes)));
                }
            }
        }
    }
}

// 从 sheets JSON 还原对象层；media_parts 提供时把 media/... 引用内联回 DataURL/SVG
fn restore_objects_from_sheets(
    st: &mut AppState,
    sheets: &Value,
    media_parts: Option<&std::collections::HashMap<String, Vec<u8>>>,
) {
    st.objects.clear();
    let arr = match sheets.as_array() {
        Some(a) => a,
        None => return,
    };
    for (i, sh) in arr.iter().enumerate() {
        let objs = match sh["objects"].as_array() {
            Some(o) => o,
            None => continue,
        };
        if objs.is_empty() {
            continue;
        }
        let mut list = Vec::new();
        for o in objs {
            let mut oj = o.clone();
            if let Some(parts) = media_parts {
                inline_object_media(&mut oj, parts);
            }
            list.push(oj);
        }
        st.objects.insert(i as u32, list);
    }
}

// 导出无损 HTML：有效行列可视化表格 + 内嵌 UniCell 数据（含无损 xlsx base64）
// 将单个对象渲染为绝对定位的可视 HTML（导出无损 HTML 覆盖层）
fn render_object_html(o: &Value, left: f64, top: f64) -> String {
    let t = o["type"].as_str().unwrap_or("");
    let w = o["w"].as_f64().unwrap_or(200.0);
    let h = o["h"].as_f64().unwrap_or(120.0);
    let cfg = &o["config"];
    let pos = format!(
        "left:{:.0}px;top:{:.0}px;width:{:.0}px;height:{:.0}px;",
        left, top, w, h
    );
    match t {
        "image" => format!(
            "<img src=\"{}\" alt=\"\" style=\"{}\">",
            html_escape(cfg["src"].as_str().unwrap_or("")),
            pos
        ),
        "svg" => format!(
            "<div style=\"{}overflow:hidden\">{}</div>",
            pos,
            cfg["svg"].as_str().unwrap_or("")
        ),
        "text" => format!(
            "<div style=\"{}background:#fff;border:1px solid #c3c9d2;border-radius:3px;padding:6px 8px;overflow:auto;font:13px sans-serif\">{}</div>",
            pos,
            cfg["html"].as_str().unwrap_or("")
        ),
        "video" => format!(
            "<video src=\"{}\" controls style=\"{}background:#000\"></video>",
            html_escape(cfg["src"].as_str().unwrap_or("")),
            pos
        ),
        "html" => format!(
            "<iframe sandbox=\"allow-scripts allow-forms allow-popups\" srcdoc=\"{}\" style=\"{}\"></iframe>",
            html_escape(cfg["code"].as_str().unwrap_or("")),
            pos
        ),
        _ => String::new(),
    }
}

fn print_cell_html(st: &AppState, sheet: u32, row: i32, col: i32, fallback: &str) -> String {
    let Some(runs) = st.rich_text.get(&(sheet, row, col)) else {
        return html_escape(fallback).replace('\n', "<br>");
    };
    if runs.is_empty() {
        return html_escape(fallback).replace('\n', "<br>");
    }
    let mut out = String::new();
    for run in runs {
        let mut css = String::new();
        if run.bold {
            css.push_str("font-weight:bold;");
        }
        if run.italic {
            css.push_str("font-style:italic;");
        }
        let mut deco = Vec::new();
        if run.underline {
            deco.push("underline");
        }
        if run.strike {
            deco.push("line-through");
        }
        if !deco.is_empty() {
            css.push_str(&format!("text-decoration:{};", deco.join(" ")));
        }
        if let Some(size) = run.size {
            if size > 0.0 {
                css.push_str(&format!("font-size:{size:.2}pt;"));
            }
        }
        if let Some(font) = &run.font {
            if !font.is_empty() {
                css.push_str(&format!(
                    "font-family:'{}';",
                    font.replace('\\', "\\\\").replace('\'', "\\'")
                ));
            }
        }
        let color = st.model.resolve_color(&run.color);
        if !color.is_empty() {
            css.push_str(&format!("color:{color};"));
        }
        out.push_str(&format!(
            "<span style=\"{}\">{}</span>",
            css,
            html_escape(&run.text).replace('\n', "<br>")
        ));
    }
    out
}

#[derive(Clone, Debug)]
struct PrintAxisBand {
    cell_start: Option<i32>,
    cell_end: Option<i32>,
    offset: f64,
    limit: f64,
}

/// Split a sheet axis at real row/column boundaries. Each band becomes one physical page axis.
fn build_print_axis_bands(
    sizes: &[f64],
    first_cell: i32,
    table_extent: f64,
    content_extent: f64,
    page_capacity: f64,
    manual_breaks: &[i32],
) -> Vec<PrintAxisBand> {
    let capacity = page_capacity.max(1.0);
    let mut bands = Vec::new();
    let mut index = 0usize;
    let mut offset = 0.0f64;
    while index < sizes.len() {
        let first_index = index;
        let band_offset = offset;
        let mut span = 0.0f64;
        while index < sizes.len() {
            // OOXML manual breaks identify the first cell on the next page.  They
            // take precedence over automatic capacity pagination, exactly as Excel.
            if index > first_index
                && manual_breaks
                    .binary_search(&(first_cell + index as i32))
                    .is_ok()
            {
                break;
            }
            let size = sizes[index].max(0.0);
            if index > first_index && span + size > capacity + 0.01 {
                break;
            }
            span += size;
            index += 1;
            // Keep an over-sized single row/column intact and guarantee forward progress.
            if span > capacity + 0.01 {
                break;
            }
        }
        bands.push(PrintAxisBand {
            cell_start: Some(first_cell + first_index as i32),
            cell_end: Some(first_cell + index as i32 - 1),
            offset: band_offset,
            limit: band_offset,
        });
        offset += span;
    }
    if bands.is_empty() {
        bands.push(PrintAxisBand {
            cell_start: None,
            cell_end: None,
            offset: 0.0,
            limit: 0.0,
        });
    }

    // A drawing may extend beyond the last used cell; continue with object-only physical pages.
    while bands
        .last()
        .map(|b| b.offset + capacity + 0.01 < content_extent)
        .unwrap_or(false)
    {
        let next = bands.last().unwrap().offset + capacity;
        bands.push(PrintAxisBand {
            cell_start: None,
            cell_end: None,
            offset: next,
            limit: next,
        });
    }
    let final_extent = table_extent.max(content_extent).max(1.0);
    for i in 0..bands.len() {
        bands[i].limit = if i + 1 < bands.len() {
            bands[i + 1].offset
        } else {
            final_extent.max(bands[i].offset + 0.01)
        };
    }
    bands
}

/// Deterministic paged-media renderer inspired by generate_bg.py: every browser/PDF page is an
/// explicit fixed-size node, and each node contains only its real row/column slice.
fn api_print_html_paged(st: &AppState, query: &str) -> Result<Resp, String> {
    if qget(query, "scope") == Some("workbook") {
        return api_print_workbook_html(st, query);
    }
    let sheet_i = qi(query, "sheet", 0).max(0) as usize;
    let wb = &st.model.get_model().workbook;
    let names = wb.get_worksheet_names();
    let name = names.get(sheet_i).ok_or("工作表不存在")?;
    let sheet = sheet_i as u32;
    let ws = wb.worksheet(sheet).map_err(|e| e.to_string())?;
    let dimension = ws.dimension();
    let objects = st.objects.get(&sheet).cloned().unwrap_or_default();

    // Materialise the lossless page-layout journal before rendering.  This makes
    // browser print/PDF observe edits immediately, without requiring an Excel export.
    let excel_print = page_review_model_with_edits(st)
        .ok()
        .map(|model| print_layout::settings_from_page_model(&model, sheet_i))
        .unwrap_or_default();

    // Excel prints every member of a disjoint Print_Area union as an independent
    // page sequence.  A small two-pass composer below obtains the exact page count
    // first, then renders headers/footers with continuous &P/&N values.
    if excel_print.print_areas.len() > 1 && qget(query, "area-index").is_none() {
        return api_print_area_union_html(st, query, excel_print.print_areas.len());
    }
    let selected_print_area = if excel_print.print_areas.is_empty() {
        None
    } else {
        let area_index = qi(query, "area-index", 0).max(0) as usize;
        excel_print.print_areas.get(area_index).copied()
    };

    let (r0, c0, mut r1, mut c1) = if let Some(area) = selected_print_area {
        (
            area.r0.clamp(1, MAX_ROWS),
            area.c0.clamp(1, MAX_COLS),
            area.r1.clamp(1, MAX_ROWS),
            area.c1.clamp(1, MAX_COLS),
        )
    } else {
        (
            1,
            1,
            dimension.max_row.max(1).min(MAX_ROWS),
            dimension.max_column.max(1).min(MAX_COLS),
        )
    };
    // A workbook print area is authoritative.  Without one, cell-anchored drawings
    // extend the useful range just as they do in Excel's print preview.
    if selected_print_area.is_none() {
        for object in &objects {
            if object["mode"].as_str().unwrap_or("cell") != "abs" {
                r1 = r1.max(object["r"].as_i64().unwrap_or(1).clamp(1, MAX_ROWS as i64) as i32);
                c1 = c1.max(object["c"].as_i64().unwrap_or(1).clamp(1, MAX_COLS as i64) as i32);
            }
        }
    }
    let print_cells = (r1 - r0 + 1) as i64 * (c1 - c0 + 1) as i64;
    if print_cells > 2_000_000 {
        return Err(format!(
            "打印区域为 {r1} 行 × {c1} 列（{print_cells} 个单元格），超过 2,000,000 单元格安全上限"
        ));
    }

    let mut col_widths = Vec::with_capacity((c1 - c0 + 1) as usize);
    let mut left_of = std::collections::HashMap::new();
    let mut table_width = 0.0f64;
    for column in c0..=c1 {
        left_of.insert(column, table_width);
        let width = st
            .model
            .get_column_width(sheet, column)
            .unwrap_or(100.0)
            .max(0.0);
        col_widths.push(width);
        table_width += width;
    }
    let mut row_heights = Vec::with_capacity((r1 - r0 + 1) as usize);
    let mut top_of = std::collections::HashMap::new();
    let mut table_height = 0.0f64;
    for row in r0..=r1 {
        top_of.insert(row, table_height);
        let height = st.model.get_row_height(sheet, row).unwrap_or(21.0).max(0.0);
        row_heights.push(height);
        table_height += height;
    }
    // Print titles are independent of Print_Area and may sit above/left of it.
    // Only the portion already at the area's leading edge is removed from the body;
    // every configured title row/column is then prepended to each physical page.
    let repeat_rows: Vec<i32> = excel_print
        .repeat_titles
        .rows
        .map(|(start, end)| (start.clamp(1, MAX_ROWS)..=end.clamp(1, MAX_ROWS)).collect())
        .unwrap_or_default();
    let repeat_columns: Vec<i32> = excel_print
        .repeat_titles
        .columns
        .map(|(start, end)| (start.clamp(1, MAX_COLS)..=end.clamp(1, MAX_COLS)).collect())
        .unwrap_or_default();
    let repeat_row_body_skip = excel_print
        .repeat_titles
        .rows
        .filter(|(start, end)| *start <= r0 && *end >= r0)
        .map(|(_, end)| (end.min(r1) - r0 + 1).max(0) as usize)
        .unwrap_or(0);
    let repeat_column_body_skip = excel_print
        .repeat_titles
        .columns
        .filter(|(start, end)| *start <= c0 && *end >= c0)
        .map(|(_, end)| (end.min(c1) - c0 + 1).max(0) as usize)
        .unwrap_or(0);
    let repeat_rows_height: f64 = repeat_rows
        .iter()
        .map(|row| {
            st.model
                .get_row_height(sheet, *row)
                .unwrap_or(21.0)
                .max(0.0)
        })
        .sum();
    let repeat_columns_width: f64 = repeat_columns
        .iter()
        .map(|column| {
            st.model
                .get_column_width(sheet, *column)
                .unwrap_or(100.0)
                .max(0.0)
        })
        .sum();
    let skipped_rows_height: f64 = row_heights.iter().take(repeat_row_body_skip).sum();
    let skipped_columns_width: f64 = col_widths.iter().take(repeat_column_body_skip).sum();

    let mut merges: Vec<(i32, i32, i32, i32)> = Vec::new();
    for merge in st.model.get_merged_cells(sheet).unwrap_or_default() {
        let Some((a, b)) = merge.split_once(':') else {
            continue;
        };
        let (Some((ar, ac)), Some((br, bc))) = (parse_a1(a), parse_a1(b)) else {
            continue;
        };
        let mr0 = ar.min(br).clamp(1, MAX_ROWS);
        let mc0 = ac.min(bc).clamp(1, MAX_COLS);
        let mr1 = ar.max(br).clamp(1, MAX_ROWS);
        let mc1 = ac.max(bc).clamp(1, MAX_COLS);
        if mr0 <= mr1 && mc0 <= mc1 {
            merges.push((mr0, mc0, mr1, mc1));
        }
    }

    let mut placements: Vec<(usize, f64, f64, f64, f64)> = Vec::new();
    let heading_width = if excel_print.print_headings {
        42.0
    } else {
        0.0
    };
    let heading_height = if excel_print.print_headings {
        20.0
    } else {
        0.0
    };
    let mut content_width = table_width + heading_width;
    let mut content_height = table_height + heading_height;
    for (object_index, object) in objects.iter().enumerate() {
        let ox = object["x"].as_f64().unwrap_or(0.0);
        let oy = object["y"].as_f64().unwrap_or(0.0);
        let object_mode = object["mode"].as_str().unwrap_or("cell");
        if object_mode != "abs" {
            let column = object["c"].as_i64().unwrap_or(1).clamp(1, MAX_COLS as i64) as i32;
            let row = object["r"].as_i64().unwrap_or(1).clamp(1, MAX_ROWS as i64) as i32;
            if selected_print_area
                .map(|area| row < area.r0 || row > area.r1 || column < area.c0 || column > area.c1)
                .unwrap_or(false)
            {
                continue;
            }
        }
        let (left, top) = if object_mode == "abs" {
            (ox, oy)
        } else {
            let column = object["c"].as_i64().unwrap_or(1).clamp(1, MAX_COLS as i64) as i32;
            let row = object["r"].as_i64().unwrap_or(1).clamp(1, MAX_ROWS as i64) as i32;
            (
                left_of.get(&column).copied().unwrap_or(table_width) + ox,
                top_of.get(&row).copied().unwrap_or(table_height) + oy,
            )
        };
        let width = object["w"].as_f64().unwrap_or(200.0).max(0.0);
        let height = object["h"].as_f64().unwrap_or(120.0).max(0.0);
        content_width = content_width.max(heading_width + left + width);
        content_height = content_height.max(heading_height + top + height);
        placements.push((object_index, left, top, width, height));
    }
    let paginated_content_width = heading_width
        + repeat_columns_width
        + (content_width - heading_width - skipped_columns_width).max(0.0);
    let paginated_content_height = heading_height
        + repeat_rows_height
        + (content_height - heading_height - skipped_rows_height).max(0.0);

    let paper = qget(query, "paper").unwrap_or("sheet").to_ascii_lowercase();
    let orientation = match qget(query, "orientation") {
        Some("landscape") => "landscape",
        Some("portrait") => "portrait",
        _ if excel_print.orientation == "landscape" => "landscape",
        _ => "portrait",
    };
    let requested_scaling = match qget(query, "scaling").unwrap_or("sheet") {
        "fit-width" => "fit-width",
        "fit-height" => "fit-height",
        "fit-sheet" => "fit-sheet",
        "sheet"
            if excel_print.fit_to_width.unwrap_or(0) > 0
                && excel_print.fit_to_height.unwrap_or(0) > 0 =>
        {
            "fit-sheet"
        }
        "sheet" if excel_print.fit_to_width.unwrap_or(0) > 0 => "fit-width",
        "sheet" if excel_print.fit_to_height.unwrap_or(0) > 0 => "fit-height",
        "sheet" if excel_print.scale_percent.is_some() => "custom",
        _ => "none",
    };
    let fixed_paper = if paper == "sheet" || paper == "custom" {
        Some(excel_print.paper_spec())
    } else {
        print_layout::paper_spec_by_name(&paper)
    };
    let px_per_mm = 96.0 / 25.4;
    let (
        paper_label,
        print_orientation,
        print_scaling,
        print_scale,
        page_width,
        page_height,
        margin_left,
        margin_right,
        margin_top,
        margin_bottom,
        page_rule,
        mut x_bands,
        y_bands,
    ) = if let Some(paper_spec) = fixed_paper {
        let label = paper_spec.label;
        let portrait_width_mm = paper_spec.width_mm;
        let portrait_height_mm = paper_spec.height_mm;
        let (width_mm, height_mm) = if orientation == "landscape" {
            (portrait_height_mm, portrait_width_mm)
        } else {
            (portrait_width_mm, portrait_height_mm)
        };
        let page_width = width_mm * px_per_mm;
        let page_height = height_mm * px_per_mm;
        let margin_left = excel_print
            .margin_left_px
            .min(page_width / 2.0 - 1.0)
            .max(0.0);
        let margin_right = excel_print
            .margin_right_px
            .min(page_width / 2.0 - 1.0)
            .max(0.0);
        let margin_top = excel_print
            .margin_top_px
            .min(page_height / 2.0 - 1.0)
            .max(0.0);
        let margin_bottom = excel_print
            .margin_bottom_px
            .min(page_height / 2.0 - 1.0)
            .max(0.0);
        let printable_width = (page_width - margin_left - margin_right).max(1.0);
        let printable_height = (page_height - margin_top - margin_bottom).max(1.0);
        // Excel's Fit-to modes only shrink; they never stretch a small sheet to fill paper.
        let width_pages = if qget(query, "scaling").unwrap_or("sheet") == "sheet" {
            excel_print.fit_to_width.unwrap_or(1).max(1) as f64
        } else {
            1.0
        };
        let height_pages = if qget(query, "scaling").unwrap_or("sheet") == "sheet" {
            excel_print.fit_to_height.unwrap_or(1).max(1) as f64
        } else {
            1.0
        };
        let width_scale = printable_width * width_pages / paginated_content_width.max(1.0);
        let height_scale = printable_height * height_pages / paginated_content_height.max(1.0);
        let print_scale = match requested_scaling {
            "fit-width" => width_scale.min(1.0),
            "fit-height" => height_scale.min(1.0),
            "fit-sheet" => width_scale.min(height_scale).min(1.0),
            "custom" => (excel_print.scale_percent.unwrap_or(100.0) / 100.0).clamp(0.1, 4.0),
            _ => 1.0,
        }
        .max(0.01);
        let source_page_width = printable_width / print_scale;
        let source_page_height = printable_height / print_scale;
        (
            label,
            orientation,
            requested_scaling,
            print_scale,
            page_width,
            page_height,
            margin_left,
            margin_right,
            margin_top,
            margin_bottom,
            format!("size:{width_mm:.3}mm {height_mm:.3}mm;margin:0"),
            {
                let mut bands = build_print_axis_bands(
                    &col_widths[repeat_column_body_skip..],
                    c0 + repeat_column_body_skip as i32,
                    (table_width - skipped_columns_width).max(0.0),
                    (content_width - skipped_columns_width - heading_width).max(0.0),
                    (source_page_width - repeat_columns_width - heading_width).max(1.0),
                    if requested_scaling == "none" || requested_scaling == "custom" {
                        &excel_print.column_breaks
                    } else {
                        &[]
                    },
                );
                for band in &mut bands {
                    band.offset += skipped_columns_width;
                    band.limit += skipped_columns_width;
                }
                bands
            },
            {
                let mut bands = build_print_axis_bands(
                    &row_heights[repeat_row_body_skip..],
                    r0 + repeat_row_body_skip as i32,
                    (table_height - skipped_rows_height).max(0.0),
                    (content_height - skipped_rows_height - heading_height).max(0.0),
                    (source_page_height - repeat_rows_height - heading_height).max(1.0),
                    if requested_scaling == "none" || requested_scaling == "custom" {
                        &excel_print.row_breaks
                    } else {
                        &[]
                    },
                );
                for band in &mut bands {
                    band.offset += skipped_rows_height;
                    band.limit += skipped_rows_height;
                }
                bands
            },
        )
    } else {
        let margin_left = 12.0f64;
        let margin_right = 12.0f64;
        let margin_top = 12.0f64;
        let margin_bottom = 12.0f64;
        let page_width = (paginated_content_width + margin_left + margin_right)
            .ceil()
            .max(96.0);
        let page_height = (paginated_content_height + margin_top + margin_bottom)
            .ceil()
            .max(96.0);
        (
            "actual".to_string(),
            "actual",
            "none",
            1.0,
            page_width,
            page_height,
            margin_left,
            margin_right,
            margin_top,
            margin_bottom,
            format!("size:{page_width:.0}px {page_height:.0}px;margin:0"),
            vec![PrintAxisBand {
                cell_start: Some(c0),
                cell_end: Some(c1),
                offset: 0.0,
                limit: (content_width - heading_width).max(1.0),
            }],
            vec![PrintAxisBand {
                cell_start: Some(r0),
                cell_end: Some(r1),
                offset: 0.0,
                limit: (content_height - heading_height).max(1.0),
            }],
        )
    };

    // A scaled worksheet can leave a trailing pagination band that contains no
    // cells or drawings (for example a narrow remainder column after fit-to-page).
    // Do not emit a completely blank physical page for that band.
    x_bands.retain(|x_band| {
        y_bands.iter().any(|y_band| {
            let cell_content = match (
                x_band.cell_start,
                x_band.cell_end,
                y_band.cell_start,
                y_band.cell_end,
            ) {
                (Some(c_start), Some(c_end), Some(r_start), Some(r_end)) => {
                    (r_start..=r_end).any(|row| {
                        (c_start..=c_end).any(|column| {
                            st.model
                                .get_formatted_cell_value(sheet, row, column)
                                .map(|value| !value.is_empty())
                                .unwrap_or(false)
                        })
                    })
                }
                _ => false,
            };
            let merge_content = merges.iter().any(|&(mr0, mc0, mr1, mc1)| {
                x_band.cell_start.map(|v| mc1 >= v).unwrap_or(false)
                    && x_band.cell_end.map(|v| mc0 <= v).unwrap_or(false)
                    && y_band.cell_start.map(|v| mr1 >= v).unwrap_or(false)
                    && y_band.cell_end.map(|v| mr0 <= v).unwrap_or(false)
            });
            let drawing_content = placements.iter().any(|&(_, left, top, width, height)| {
                left < x_band.limit
                    && left + width > x_band.offset
                    && top < y_band.limit
                    && top + height > y_band.offset
            });
            cell_content || merge_content || drawing_content
        })
    });
    if x_bands.is_empty() {
        x_bands.push(PrintAxisBand {
            cell_start: None,
            cell_end: None,
            offset: 0.0,
            limit: 0.0,
        });
    }
    let final_x_extent = content_width.max(1.0);
    if let Some(last) = x_bands.last_mut() {
        last.limit = final_x_extent.max(last.offset + 0.01);
    }
    let printable_width = (page_width - margin_left - margin_right).max(1.0);
    let printable_height = (page_height - margin_top - margin_bottom).max(1.0);
    let source_page_width = printable_width / print_scale;
    let source_page_height = printable_height / print_scale;
    let page_surface_transform = if (print_scale - 1.0).abs() > 0.000001 {
        format!("transform:scale({print_scale:.8});")
    } else {
        String::new()
    };
    let page_name = qget(query, "page-name")
        .map(|value| {
            value
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
                .take(64)
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unicell-paper".to_string());
    let (header_left, header_width) = if excel_print.align_header_footer_with_margins {
        (margin_left, printable_width)
    } else {
        (0.0, page_width)
    };
    let header_font_size = if excel_print.scale_header_footer {
        9.0 * print_scale
    } else {
        9.0
    };
    let mut print_class_names = Vec::new();
    if excel_print.print_grid_lines {
        print_class_names.push("print-gridlines");
    }
    if excel_print.black_and_white {
        print_class_names.push("print-black-and-white");
    }
    if excel_print.draft {
        print_class_names.push("print-draft");
    }
    let print_classes = print_class_names.join(" ");
    let page_classes = if print_classes.is_empty() {
        "print-page".to_string()
    } else {
        format!("print-page {print_classes}")
    };

    let body_page_count = x_bands.len().saturating_mul(y_bands.len()).max(1);
    let printable_notes: Vec<&print_layout::PrintNote> = excel_print
        .notes
        .iter()
        .filter(|note| {
            excel_print.print_areas.is_empty()
                || excel_print.print_areas.iter().any(|area| {
                    note.row >= area.r0
                        && note.row <= area.r1
                        && note.column >= area.c0
                        && note.column <= area.c1
                })
        })
        .collect();
    let include_comment_appendix = excel_print.printed_comments
        == print_layout::PrintedComments::AtEnd
        && qget(query, "comments") != Some("0")
        && !printable_notes.is_empty();
    let comment_page_ranges = if include_comment_appendix {
        let line_capacity = ((printable_height - 48.0) / 17.0).floor().max(6.0) as usize;
        let costs: Vec<usize> = printable_notes
            .iter()
            .map(|note| {
                let characters = note.reference.chars().count()
                    + note.author.chars().count()
                    + note.text.chars().count();
                2 + characters.div_ceil(84)
            })
            .collect();
        print_layout::paginate_item_costs(&costs, line_capacity)
    } else {
        Vec::new()
    };
    let comment_page_count = comment_page_ranges.len();
    let local_page_count = body_page_count + comment_page_count;
    let page_offset = qi(query, "page-offset", 0).max(0) as usize;
    let total_page_count = qi(query, "page-total", 0)
        .max(0)
        .try_into()
        .ok()
        .filter(|value: &usize| *value > 0)
        .unwrap_or(page_offset + local_page_count);
    let page_coordinates = print_layout::ordered_page_coordinates(
        y_bands.len(),
        x_bands.len(),
        excel_print.page_order,
    );
    let mut pages_html = String::new();
    let mut page_number = 0usize;
    for (page_row, page_col) in page_coordinates {
        let y_band = &y_bands[page_row];
        let x_band = &x_bands[page_col];
        page_number += 1;
        let mut table_html = String::new();
        if let (Some(pr0), Some(pr1), Some(pc0), Some(pc1)) = (
            y_band.cell_start,
            y_band.cell_end,
            x_band.cell_start,
            x_band.cell_end,
        ) {
            let mut print_rows = repeat_rows.clone();
            for row in pr0..=pr1 {
                if !print_rows.contains(&row) {
                    print_rows.push(row);
                }
            }
            let mut print_columns = repeat_columns.clone();
            for column in pc0..=pc1 {
                if !print_columns.contains(&column) {
                    print_columns.push(column);
                }
            }
            // A merge crossing a page boundary becomes a clipped merge fragment. Each fragment
            // keeps the original merge-origin value/style, so no page silently loses content.
            let mut local_origins: std::collections::HashMap<(i32, i32), (i32, i32, i32, i32)> =
                std::collections::HashMap::new();
            let mut local_covered: std::collections::HashSet<(i32, i32)> =
                std::collections::HashSet::new();
            for &(mr0, mc0, mr1, mc1) in &merges {
                let selected_rows: Vec<i32> = print_rows
                    .iter()
                    .copied()
                    .filter(|row| *row >= mr0 && *row <= mr1)
                    .collect();
                let selected_columns: Vec<i32> = print_columns
                    .iter()
                    .copied()
                    .filter(|column| *column >= mc0 && *column <= mc1)
                    .collect();
                if selected_rows.is_empty() || selected_columns.is_empty() {
                    continue;
                }
                let ir0 = selected_rows[0];
                let ic0 = selected_columns[0];
                local_origins.insert(
                    (ir0, ic0),
                    (
                        selected_rows.len() as i32,
                        selected_columns.len() as i32,
                        mr0,
                        mc0,
                    ),
                );
                for &row in &selected_rows {
                    for &column in &selected_columns {
                        if row != ir0 || column != ic0 {
                            local_covered.insert((row, column));
                        }
                    }
                }
            }

            let local_width: f64 = print_columns
                .iter()
                .map(|column| {
                    st.model
                        .get_column_width(sheet, *column)
                        .unwrap_or(100.0)
                        .max(0.0)
                })
                .sum::<f64>()
                + heading_width;
            table_html.push_str(&format!(
                "<table aria-label=\"工作表打印区域\" style=\"width:{local_width:.2}px\"><colgroup>"
            ));
            if excel_print.print_headings {
                table_html.push_str(&format!("<col style=\"width:{heading_width:.2}px\">"));
            }
            for &column in &print_columns {
                table_html.push_str(&format!(
                    "<col style=\"width:{:.2}px\">",
                    st.model
                        .get_column_width(sheet, column)
                        .unwrap_or(100.0)
                        .max(0.0)
                ));
            }
            table_html.push_str("</colgroup>");
            if excel_print.print_headings {
                table_html.push_str(&format!(
                        "<thead><tr style=\"height:{heading_height:.2}px\"><th class=\"print-corner-heading\"></th>"
                    ));
                for &column in &print_columns {
                    table_html.push_str(&format!(
                        "<th scope=\"col\">{}</th>",
                        num_to_col(column as i64)
                    ));
                }
                table_html.push_str("</tr></thead>");
            }
            table_html.push_str("<tbody>");
            for &row in &print_rows {
                table_html.push_str(&format!(
                    "<tr style=\"height:{:.2}px\">",
                    st.model.get_row_height(sheet, row).unwrap_or(21.0).max(0.0)
                ));
                if excel_print.print_headings {
                    table_html.push_str(&format!("<th scope=\"row\">{row}</th>"));
                }
                for &column in &print_columns {
                    if local_covered.contains(&(row, column)) {
                        continue;
                    }
                    let (rowspan_n, colspan_n, source_row, source_column) = local_origins
                        .get(&(row, column))
                        .copied()
                        .unwrap_or((1, 1, row, column));
                    let formatted = st
                        .model
                        .get_formatted_cell_value(sheet, source_row, source_column)
                        .unwrap_or_default();
                    let formatted =
                        print_layout::printed_cell_value(&formatted, excel_print.printed_errors);
                    let style = st
                        .model
                        .get_model()
                        .get_cell_style_or_none(sheet, source_row, source_column)
                        .ok()
                        .flatten();
                    let style_json = style
                        .as_ref()
                        .map(|s| style_to_json(st, s))
                        .unwrap_or(Value::Null);
                    let css = if style_json.is_null() {
                        String::new()
                    } else {
                        style_css_from_json(&style_json)
                    };
                    let allow_text_overflow = !formatted.is_empty()
                        && print_columns
                            .iter()
                            .skip_while(|candidate| **candidate != column)
                            .skip(1)
                            .all(|candidate| {
                                st.model
                                    .get_formatted_cell_value(sheet, source_row, *candidate)
                                    .map(|value| value.is_empty())
                                    .unwrap_or(true)
                            });
                    let overflow_css = if allow_text_overflow {
                        "overflow:visible;"
                    } else {
                        "overflow:hidden;"
                    };
                    let rowspan = if rowspan_n > 1 {
                        format!(" rowspan=\"{rowspan_n}\"")
                    } else {
                        String::new()
                    };
                    let colspan = if colspan_n > 1 {
                        format!(" colspan=\"{colspan_n}\"")
                    } else {
                        String::new()
                    };
                    let note_html = if excel_print.printed_comments
                        == print_layout::PrintedComments::AsDisplayed
                    {
                        excel_print
                                .notes
                                .iter()
                                .find(|note| note.row == source_row && note.column == source_column)
                                .map(|note| format!(
                                    "<aside class=\"print-note-callout\" data-note-ref=\"{}\"><strong>{}</strong><div>{}</div></aside>",
                                    html_escape(&note.reference),
                                    html_escape(&note.author),
                                    html_escape(&note.text).replace('\n', "<br>")
                                ))
                                .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let note_class = if note_html.is_empty() {
                        ""
                    } else {
                        " class=\"has-print-note\""
                    };
                    table_html.push_str(&format!(
                        "<td{note_class}{rowspan}{colspan} style=\"{overflow_css}{css}\">{}{note_html}</td>",
                        print_cell_html(st, sheet, source_row, source_column, &formatted)
                    ));
                }
                table_html.push_str("</tr>");
            }
            table_html.push_str("</tbody></table>");
        }

        let mut objects_html = String::new();
        for &(object_index, left, top, width, height) in &placements {
            if left < x_band.limit
                && left + width > x_band.offset
                && top < y_band.limit
                && top + height > y_band.offset
            {
                objects_html.push_str(&render_object_html(
                    &objects[object_index],
                    left - x_band.offset + repeat_columns_width + heading_width,
                    top - y_band.offset + repeat_rows_height + heading_height,
                ));
            }
        }
        if !objects_html.is_empty() {
            objects_html = format!("<div class=\"print-objects\">{objects_html}</div>");
        }
        let global_page_index = page_offset + page_number;
        let displayed_page_number = excel_print.displayed_page_number(global_page_index - 1);
        let (header_source, footer_source) =
            if global_page_index == 1 && excel_print.different_first {
                (&excel_print.first_header, &excel_print.first_footer)
            } else if displayed_page_number % 2 == 0 && excel_print.different_odd_even {
                (&excel_print.even_header, &excel_print.even_footer)
            } else {
                (&excel_print.odd_header, &excel_print.odd_footer)
            };
        let header = print_layout::render_header_footer_html(
            header_source,
            name,
            &st.file_name,
            displayed_page_number,
            total_page_count,
        );
        let footer = print_layout::render_header_footer_html(
            footer_source,
            name,
            &st.file_name,
            displayed_page_number,
            total_page_count,
        );
        let header_html = format!(
            "<div class=\"page-header\" style=\"left:{header_left:.3}px;width:{header_width:.3}px;top:{:.3}px;font-size:{header_font_size:.3}pt\"><span>{}</span><span>{}</span><span>{}</span></div>",
            excel_print.header_px, header[0], header[1], header[2],
        );
        let footer_html = format!(
            "<div class=\"page-footer\" style=\"left:{header_left:.3}px;width:{header_width:.3}px;bottom:{:.3}px;font-size:{header_font_size:.3}pt\"><span>{}</span><span>{}</span><span>{}</span></div>",
            excel_print.footer_px, footer[0], footer[1], footer[2],
        );
        let source_width = heading_width + repeat_columns_width + x_band.limit - x_band.offset;
        let source_height = heading_height + repeat_rows_height + y_band.limit - y_band.offset;
        let centered_left = if excel_print.horizontal_centered {
            ((printable_width - source_width * print_scale) / 2.0).max(0.0)
        } else {
            0.0
        };
        let centered_top = if excel_print.vertical_centered {
            ((printable_height - source_height * print_scale) / 2.0).max(0.0)
        } else {
            0.0
        };
        let area_index = qi(query, "area-index", 0).max(0);
        pages_html.push_str(&format!(
                "<section class=\"{page_classes} page-node-{global_page_index}\" style=\"width:{page_width:.3}px;height:{page_height:.3}px;page:{page_name}\" data-page-number=\"{displayed_page_number}\" data-page-index=\"{global_page_index}\" data-print-area=\"{area_index}\" data-page-row=\"{}\" data-page-col=\"{}\" data-source-left=\"{:.2}\" data-source-top=\"{:.2}\" data-source-width=\"{:.2}\" data-source-height=\"{:.2}\">{header_html}<div class=\"page-content\" style=\"left:{margin_left:.3}px;top:{margin_top:.3}px;width:{printable_width:.3}px;height:{printable_height:.3}px\"><div class=\"page-surface\" style=\"width:{source_page_width:.3}px;height:{source_page_height:.3}px;transform-origin:0 0;{page_surface_transform}left:{centered_left:.3}px;top:{centered_top:.3}px\">{table_html}{objects_html}</div></div>{footer_html}</section>",
                page_row + 1,
                page_col + 1,
                x_band.offset,
                y_band.offset,
                x_band.limit - x_band.offset,
                y_band.limit - y_band.offset,
            ));
    }

    if include_comment_appendix {
        for (start, end) in comment_page_ranges {
            let notes = &printable_notes[start..end];
            page_number += 1;
            let global_page_index = page_offset + page_number;
            let displayed_page_number = excel_print.displayed_page_number(global_page_index - 1);
            let (header_source, footer_source) =
                if global_page_index == 1 && excel_print.different_first {
                    (&excel_print.first_header, &excel_print.first_footer)
                } else if displayed_page_number % 2 == 0 && excel_print.different_odd_even {
                    (&excel_print.even_header, &excel_print.even_footer)
                } else {
                    (&excel_print.odd_header, &excel_print.odd_footer)
                };
            let header = print_layout::render_header_footer_html(
                header_source,
                name,
                &st.file_name,
                displayed_page_number,
                total_page_count,
            );
            let footer = print_layout::render_header_footer_html(
                footer_source,
                name,
                &st.file_name,
                displayed_page_number,
                total_page_count,
            );
            let header_html = format!(
                "<div class=\"page-header\" style=\"left:{header_left:.3}px;width:{header_width:.3}px;top:{:.3}px;font-size:{header_font_size:.3}pt\"><span>{}</span><span>{}</span><span>{}</span></div>",
                excel_print.header_px, header[0], header[1], header[2],
            );
            let footer_html = format!(
                "<div class=\"page-footer\" style=\"left:{header_left:.3}px;width:{header_width:.3}px;bottom:{:.3}px;font-size:{header_font_size:.3}pt\"><span>{}</span><span>{}</span><span>{}</span></div>",
                excel_print.footer_px, footer[0], footer[1], footer[2],
            );
            let mut notes_html = String::from("<h1>Cell comments</h1><ol>");
            for note in notes {
                notes_html.push_str(&format!(
                    "<li data-note-ref=\"{}\"><strong>{}</strong>{}<div>{}</div></li>",
                    html_escape(&note.reference),
                    html_escape(&note.reference),
                    if note.author.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", html_escape(&note.author))
                    },
                    html_escape(&note.text).replace('\n', "<br>"),
                ));
            }
            notes_html.push_str("</ol>");
            pages_html.push_str(&format!(
                "<section class=\"{page_classes} page-node-{global_page_index}\" style=\"width:{page_width:.3}px;height:{page_height:.3}px;page:{page_name}\" data-page-number=\"{displayed_page_number}\" data-page-index=\"{global_page_index}\" data-page-kind=\"comments\">{header_html}<div class=\"page-content\" style=\"left:{margin_left:.3}px;top:{margin_top:.3}px;width:{printable_width:.3}px;height:{printable_height:.3}px\"><div class=\"print-comment-appendix\">{notes_html}</div></div>{footer_html}</section>"
            ));
        }
    }

    let autoprint = qget(query, "autoprint") != Some("0");
    let return_to_app = qget(query, "return") == Some("1");
    let date_time_script = "(()=>{const now=new Date();document.querySelectorAll('[data-excel-field=date]').forEach(n=>n.textContent=now.toLocaleDateString());document.querySelectorAll('[data-excel-field=time]').forEach(n=>n.textContent=now.toLocaleTimeString())})();";
    let print_script = if autoprint {
        let return_script = if return_to_app {
            "window.addEventListener('afterprint',()=>{if(history.length>1)history.back()},{once:true});"
        } else {
            ""
        };
        format!(
            "<script>{date_time_script}{return_script}window.addEventListener('load',async()=>{{try{{if(document.fonts)await document.fonts.ready;await Promise.all(Array.from(document.images).map(img=>img.complete?Promise.resolve():new Promise(r=>{{img.addEventListener('load',r,{{once:true}});img.addEventListener('error',r,{{once:true}})}})))}}catch(e){{}}setTimeout(()=>{{window.focus();window.print()}},450)}});</script>"
        )
    } else {
        format!("<script>{date_time_script}</script>")
    };
    let page_count = page_number;
    let page_order = match excel_print.page_order {
        print_layout::PageOrder::DownThenOver => "downThenOver",
        print_layout::PageOrder::OverThenDown => "overThenDown",
    };
    let printed_errors = match excel_print.printed_errors {
        print_layout::PrintedErrors::Displayed => "displayed",
        print_layout::PrintedErrors::Blank => "blank",
        print_layout::PrintedErrors::Dash => "dash",
        print_layout::PrintedErrors::NotAvailable => "NA",
    };
    let printed_comments = match excel_print.printed_comments {
        print_layout::PrintedComments::None => "none",
        print_layout::PrintedComments::AsDisplayed => "asDisplayed",
        print_layout::PrintedComments::AtEnd => "atEnd",
    };
    let html = format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>{title} - 打印</title><style>\
*{{box-sizing:border-box}}\
html,body{{margin:0;padding:0;background:#fff;color:#000}}\
body{{font-family:Calibri,'Microsoft YaHei',sans-serif;-webkit-print-color-adjust:exact;print-color-adjust:exact}}\
.print-document{{margin:0;padding:0}}\
.print-page{{position:relative;width:{page_width:.3}px;height:{page_height:.3}px;margin:0 auto;overflow:hidden;background:#fff;break-after:page;page-break-after:always;page:{page_name}}}\
.print-page:last-child{{break-after:auto;page-break-after:auto}}\
.page-content{{position:absolute;left:{margin_left:.3}px;top:{margin_top:.3}px;width:{printable_width:.3}px;height:{printable_height:.3}px;overflow:hidden}}\
.page-surface{{position:relative;width:{source_page_width:.3}px;height:{source_page_height:.3}px;transform-origin:0 0;{page_surface_transform}}}\
.page-header,.page-footer{{position:absolute;left:{header_left:.3}px;width:{header_width:.3}px;display:grid;grid-template-columns:1fr 1fr 1fr;align-items:center;font-size:{header_font_size:.3}pt;line-height:1.1;white-space:pre;overflow:hidden}}\
.page-header{{top:{header_top:.3}px}}.page-footer{{bottom:{footer_bottom:.3}px}}\
.page-header>span:nth-child(2),.page-footer>span:nth-child(2){{text-align:center}}.page-header>span:nth-child(3),.page-footer>span:nth-child(3){{text-align:right}}\
.hf-picture-missing{{display:none!important}}\
table{{position:relative;z-index:1;border-collapse:collapse;table-layout:fixed}}\
tr{{break-inside:avoid}}\
td{{position:relative;border:0;padding:0 4px;font-size:11pt;line-height:1.2;overflow:hidden;white-space:pre;vertical-align:bottom}}\
td.has-print-note{{overflow:visible}}\
th{{padding:0 4px;border:1px solid #c9cdd2;background:#f2f2f2;color:#555;font:9pt Calibri,sans-serif;text-align:center}}\
body.print-gridlines td,.print-page.print-gridlines td{{border:1px solid #d9d9d9}}\
.print-note-callout{{position:absolute;z-index:5;left:80%;top:70%;min-width:160px;max-width:280px;padding:6px;white-space:normal;background:#fff4b8;border:1px solid #b8a252;box-shadow:1px 1px 3px #888;font:9pt Calibri,sans-serif}}\
.print-note-callout strong{{display:block;margin-bottom:2px}}\
.print-comment-appendix{{padding:8px 12px;font:10pt Calibri,'Microsoft YaHei',sans-serif}}\
.print-comment-appendix h1{{font-size:14pt;margin:0 0 10px}}.print-comment-appendix ol{{margin:0;padding-left:28px}}.print-comment-appendix li{{margin:0 0 10px;break-inside:avoid}}\
.print-objects{{position:absolute;z-index:2;left:0;top:0;width:0;height:0}}\
.print-objects>*{{position:absolute;box-sizing:border-box}}\
.print-objects img{{object-fit:contain}}\
.print-objects iframe{{border:1px solid #c3c9d2;background:#fff}}\
body.print-black-and-white .print-page,.print-page.print-black-and-white{{filter:grayscale(1)}}\
body.print-draft .print-objects,.print-page.print-draft .print-objects{{display:none!important}}body.print-draft td,.print-page.print-draft td{{background:transparent!important;box-shadow:none!important;text-shadow:none!important}}\
@media screen{{body{{background:#d8dbe0;padding:12px 0}}.print-page{{box-shadow:0 1px 5px #7d828a;margin:0 auto 12px}}}}\
@page {page_name}{{{page_rule}}}\
@media print{{html,body,.print-document{{width:auto;height:auto;background:#fff}}.print-page{{margin:0!important;border:0!important;box-shadow:none!important;transform:none!important}}}}\
</style></head><body class=\"{print_classes}\" data-unicell-print-only=\"true\" data-print-paper=\"{paper_label}\" data-print-orientation=\"{print_orientation}\" data-print-scaling=\"{print_scaling}\" data-print-scale=\"{print_scale:.8}\" data-print-pages=\"{page_count}\" data-sheet=\"{sheet}\" data-print-row-start=\"{r0}\" data-print-col-start=\"{c0}\" data-print-rows=\"{print_rows}\" data-print-cols=\"{print_cols}\" data-print-width=\"{content_width:.2}\" data-print-height=\"{content_height:.2}\" data-page-width=\"{page_width:.3}\" data-page-height=\"{page_height:.3}\" data-page-name=\"{page_name}\" data-page-order=\"{page_order}\" data-first-page-number=\"{first_page_number}\" data-use-first-page-number=\"{use_first_page_number}\" data-black-and-white=\"{black_and_white}\" data-draft=\"{draft}\" data-print-errors=\"{printed_errors}\" data-print-comments=\"{printed_comments}\" data-print-headings=\"{print_headings}\" data-print-copies=\"{copies}\" data-horizontal-dpi=\"{horizontal_dpi}\" data-vertical-dpi=\"{vertical_dpi}\"><main class=\"print-document\">{pages_html}</main>{print_script}</body></html>",
        title = html_escape(name),
        header_top = excel_print.header_px,
        footer_bottom = excel_print.footer_px,
        first_page_number = excel_print.first_page_number,
        use_first_page_number = excel_print.use_first_page_number,
        black_and_white = excel_print.black_and_white,
        draft = excel_print.draft,
        print_headings = excel_print.print_headings,
        copies = excel_print.copies,
        horizontal_dpi = excel_print
            .horizontal_dpi
            .map(|value| value.to_string())
            .unwrap_or_default(),
        vertical_dpi = excel_print
            .vertical_dpi
            .map(|value| value.to_string())
            .unwrap_or_default(),
        content_width = paginated_content_width,
        content_height = paginated_content_height,
        print_rows = r1 - r0 + 1,
        print_cols = c1 - c0 + 1,
    );
    Ok(tiny_http::Response::from_data(html.into_bytes())
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap(),
        ))
}

fn print_query_without(query: &str, excluded: &[&str]) -> Vec<String> {
    query
        .split('&')
        .filter(|item| {
            let key = item.split_once('=').map(|(key, _)| key).unwrap_or(*item);
            !excluded.contains(&key)
        })
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn print_document_pages(document: &str) -> Result<&str, String> {
    let marker = "<main class=\"print-document\">";
    let start = document
        .find(marker)
        .ok_or("print document has no page container")?
        + marker.len();
    let end = document[start..]
        .find("</main>")
        .map(|offset| start + offset)
        .ok_or("print document has no closing page container")?;
    Ok(&document[start..end])
}

fn replace_print_data_attribute(document: &mut String, name: &str, value: &str) {
    let prefix = format!("{name}=\"");
    let Some(start) = document.find(&prefix).map(|offset| offset + prefix.len()) else {
        return;
    };
    let Some(end) = document[start..].find('"').map(|offset| start + offset) else {
        return;
    };
    document.replace_range(start..end, value);
}

fn print_data_attribute<'a>(document: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=\"");
    let start = document.find(&prefix)? + prefix.len();
    let end = document[start..].find('"')? + start;
    Some(&document[start..end])
}

/// Render disjoint members of `_xlnm.Print_Area` independently.  The first pass
/// counts physical pages; the second pass supplies a global offset/total so page
/// order, first-page numbering and header/footer fields remain Excel-compatible.
fn api_print_area_union_html(
    st: &AppState,
    query: &str,
    area_count: usize,
) -> Result<Resp, String> {
    let base = print_query_without(
        query,
        &[
            "area-index",
            "page-offset",
            "page-total",
            "comments",
            "autoprint",
            "return",
            "scope",
        ],
    );
    let parameters_for = |area_index: usize, page_offset: usize, page_total: usize| {
        let mut parameters = base.clone();
        parameters.push("scope=sheet".to_string());
        parameters.push(format!("area-index={area_index}"));
        parameters.push(format!("page-offset={page_offset}"));
        if page_total > 0 {
            parameters.push(format!("page-total={page_total}"));
        }
        parameters.push(format!(
            "comments={}",
            if area_index + 1 == area_count { 1 } else { 0 }
        ));
        parameters.push("autoprint=0".to_string());
        parameters.join("&")
    };

    let mut area_pages = Vec::with_capacity(area_count);
    let mut total_pages = 0usize;
    for area_index in 0..area_count {
        let response = api_print_html_paged(st, &parameters_for(area_index, 0, 0))?;
        let mut document = String::new();
        response
            .into_reader()
            .read_to_string(&mut document)
            .map_err(|error| error.to_string())?;
        let count = print_document_pages(&document)?
            .matches("<section class=\"print-page")
            .count();
        area_pages.push(count);
        total_pages += count;
    }

    let mut first_document = String::new();
    let mut combined_pages = String::new();
    let mut page_offset = 0usize;
    for (area_index, count) in area_pages.into_iter().enumerate() {
        let response =
            api_print_html_paged(st, &parameters_for(area_index, page_offset, total_pages))?;
        let mut document = String::new();
        response
            .into_reader()
            .read_to_string(&mut document)
            .map_err(|error| error.to_string())?;
        combined_pages.push_str(print_document_pages(&document)?);
        if first_document.is_empty() {
            first_document = document;
        }
        page_offset += count;
    }
    let marker = "<main class=\"print-document\">";
    let start = first_document.find(marker).unwrap() + marker.len();
    let end = first_document[start..]
        .find("</main>")
        .map(|offset| start + offset)
        .unwrap();
    first_document.replace_range(start..end, &combined_pages);
    replace_print_data_attribute(
        &mut first_document,
        "data-print-pages",
        &total_pages.to_string(),
    );
    first_document = first_document.replace(
        "data-unicell-print-only=\"true\"",
        &format!("data-unicell-print-only=\"true\" data-print-area-count=\"{area_count}\""),
    );
    if qget(query, "autoprint") != Some("0") {
        let return_script = if qget(query, "return") == Some("1") {
            "window.addEventListener('afterprint',()=>{if(history.length>1)history.back()},{once:true});"
        } else {
            ""
        };
        let script = format!(
            "<script>{return_script}window.addEventListener('load',async()=>{{try{{if(document.fonts)await document.fonts.ready;await Promise.all(Array.from(document.images).map(img=>img.complete?Promise.resolve():new Promise(r=>{{img.addEventListener('load',r,{{once:true}});img.addEventListener('error',r,{{once:true}})}})))}}catch(e){{}}setTimeout(()=>{{window.focus();window.print()}},450)}});</script>"
        );
        first_document = first_document.replace("</body>", &format!("{script}</body>"));
    }
    Ok(tiny_http::Response::from_data(first_document.into_bytes())
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap(),
        ))
}

/// Compose every worksheet through the exact same paged renderer used by the
/// current-sheet button and Ctrl+P.  Explicit UI paper settings naturally give all
/// sheets one physical paper; `paper=sheet` uses the first sheet's CSS page box while
/// preserving every sheet's own ranges, breaks, titles, cells, drawings and headers.
fn api_print_workbook_html(st: &AppState, query: &str) -> Result<Resp, String> {
    let sheet_count = st.model.get_model().workbook.get_worksheet_names().len();
    if sheet_count == 0 {
        return Err("workbook has no worksheets".to_string());
    }
    let mut first_document = String::new();
    let mut combined_pages = String::new();
    let mut page_rules = String::new();
    let mut page_count = 0usize;
    for sheet in 0..sheet_count {
        let mut parameters: Vec<String> = query
            .split('&')
            .filter(|item| {
                let key = item.split_once('=').map(|(key, _)| key).unwrap_or(*item);
                !matches!(
                    key,
                    "sheet" | "scope" | "autoprint" | "return" | "page-name"
                )
            })
            .map(str::to_string)
            .collect();
        parameters.push(format!("sheet={sheet}"));
        parameters.push(format!("page-name=unicell-sheet-{sheet}"));
        parameters.push("scope=sheet".to_string());
        parameters.push("autoprint=0".to_string());
        let response = api_print_html_paged(st, &parameters.join("&"))?;
        let mut document = String::new();
        response
            .into_reader()
            .read_to_string(&mut document)
            .map_err(|error| error.to_string())?;
        let page_name = print_data_attribute(&document, "data-page-name")
            .ok_or("print document has no page name")?;
        let page_width = print_data_attribute(&document, "data-page-width")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or("print document has an invalid page width")?;
        let page_height = print_data_attribute(&document, "data-page-height")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or("print document has an invalid page height")?;
        page_rules.push_str(&format!(
            "@page {page_name}{{size:{page_width:.3}px {page_height:.3}px;margin:0}}"
        ));
        let marker = "<main class=\"print-document\">";
        let start = document
            .find(marker)
            .ok_or("print document has no page container")?
            + marker.len();
        let end = document[start..]
            .find("</main>")
            .map(|offset| start + offset)
            .ok_or("print document has no closing page container")?;
        combined_pages.push_str(&document[start..end]);
        page_count += document[start..end]
            .matches("<section class=\"print-page")
            .count();
        if first_document.is_empty() {
            first_document = document;
        }
    }
    let marker = "<main class=\"print-document\">";
    let start = first_document.find(marker).unwrap() + marker.len();
    let end = first_document[start..]
        .find("</main>")
        .map(|offset| start + offset)
        .unwrap();
    first_document.replace_range(start..end, &combined_pages);
    let style_end = first_document
        .find("</style>")
        .ok_or("print document has no stylesheet")?;
    first_document.insert_str(style_end, &page_rules);
    replace_print_data_attribute(
        &mut first_document,
        "data-print-pages",
        &page_count.to_string(),
    );
    first_document = first_document.replace(
        "data-unicell-print-only=\"true\"",
        &format!("data-unicell-print-only=\"true\" data-print-scope=\"workbook\" data-workbook-pages=\"{page_count}\""),
    );
    let autoprint = qget(query, "autoprint") != Some("0");
    if autoprint {
        let return_script = if qget(query, "return") == Some("1") {
            "window.addEventListener('afterprint',()=>{if(history.length>1)history.back()},{once:true});"
        } else {
            ""
        };
        let script = format!(
            "<script>{return_script}window.addEventListener('load',async()=>{{try{{if(document.fonts)await document.fonts.ready;await Promise.all(Array.from(document.images).map(img=>img.complete?Promise.resolve():new Promise(r=>{{img.addEventListener('load',r,{{once:true}});img.addEventListener('error',r,{{once:true}})}})))}}catch(e){{}}setTimeout(()=>{{window.focus();window.print()}},450)}});</script>"
        );
        first_document = first_document.replace("</body>", &format!("{script}</body>"));
    }
    Ok(tiny_http::Response::from_data(first_document.into_bytes())
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap(),
        ))
}

fn api_export_html(st: &AppState, query: &str) -> Result<Resp, String> {
    let basename = export_basename(st, query);
    let xlsx = model_to_preserved_xlsx_bytes(st)?;
    let xlsx_b64 = b64_encode(&xlsx);
    let wb = &st.model.get_model().workbook;
    let names = wb.get_worksheet_names();
    let mut body_html = String::new();
    let mut data_sheets = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let sheet = i as u32;
        let ws = wb.worksheet(sheet).map_err(|e| e.to_string())?;
        let d = ws.dimension();
        // 有效行列（视觉表格防止过大，上限 2000×200；嵌入 xlsx 仍为完整无损）
        let r0 = d.min_row.max(1);
        let c0 = d.min_column.max(1);
        let r1 = d.max_row.min(r0 + 1999);
        let c1 = d.max_column.min(c0 + 199);
        // 列宽 / 行高 + 前缀偏移（用于对象定位与表格对齐）
        let mut col_w: Vec<f64> = Vec::new();
        let mut left_of: std::collections::HashMap<i32, f64> = std::collections::HashMap::new();
        let mut accx = 0.0f64;
        for c in c0..=c1 {
            left_of.insert(c, accx);
            let w = st.model.get_column_width(sheet, c).unwrap_or(100.0);
            col_w.push(w);
            accx += w;
        }
        let mut row_h: Vec<f64> = Vec::new();
        let mut top_of: std::collections::HashMap<i32, f64> = std::collections::HashMap::new();
        let mut accy = 0.0f64;
        for r in r0..=r1 {
            top_of.insert(r, accy);
            let h = st.model.get_row_height(sheet, r).unwrap_or(21.0);
            row_h.push(h);
            accy += h;
        }
        body_html.push_str(&format!(
            "<h3>{}</h3>\n<div class=\"tbl-box\">\n<table><colgroup>",
            html_escape(name)
        ));
        for w in &col_w {
            body_html.push_str(&format!("<col style=\"width:{:.0}px\">", w));
        }
        body_html.push_str("</colgroup>\n");
        let mut cells = Vec::new();
        for (ri, r) in (r0..=r1).enumerate() {
            body_html.push_str(&format!("<tr style=\"height:{:.0}px\">", row_h[ri]));
            for c in c0..=c1 {
                let content = st.model.get_cell_content(sheet, r, c).unwrap_or_default();
                let formatted = st
                    .model
                    .get_formatted_cell_value(sheet, r, c)
                    .unwrap_or_default();
                let style = st
                    .model
                    .get_model()
                    .get_cell_style_or_none(sheet, r, c)
                    .ok()
                    .flatten();
                let sj = style
                    .as_ref()
                    .map(|s| style_to_json(st, s))
                    .unwrap_or(Value::Null);
                let css = if sj.is_null() {
                    String::new()
                } else {
                    style_css_from_json(&sj)
                };
                body_html.push_str(&format!(
                    "<td style=\"{}\">{}</td>",
                    css,
                    html_escape(&formatted)
                ));
                if !content.is_empty() {
                    cells.push(json!({"r":r,"c":c,"v":content,"f":formatted,"s":sj}));
                }
            }
            body_html.push_str("</tr>\n");
        }
        body_html.push_str("</table>\n");
        // 对象覆盖层：矢量(内联SVG)/图片/文本/视频/沙盒HTML(可运行脚本) 可视化还原（对齐 unidoc）
        let objects = st.objects.get(&sheet).cloned().unwrap_or_default();
        if !objects.is_empty() {
            body_html.push_str("<div class=\"obj-ov\">");
            for o in &objects {
                let mode = o["mode"].as_str().unwrap_or("cell");
                let ox = o["x"].as_f64().unwrap_or(0.0);
                let oy = o["y"].as_f64().unwrap_or(0.0);
                let (left, top) = if mode == "abs" {
                    (ox, oy)
                } else {
                    let cc = o["c"].as_i64().unwrap_or(1) as i32;
                    let rr = o["r"].as_i64().unwrap_or(1) as i32;
                    (
                        left_of.get(&cc).copied().unwrap_or(0.0) + ox,
                        top_of.get(&rr).copied().unwrap_or(0.0) + oy,
                    )
                };
                body_html.push_str(&render_object_html(o, left, top));
            }
            body_html.push_str("</div>");
        }
        body_html.push_str("</div>\n");
        let merges = st.model.get_merged_cells(sheet).unwrap_or_default();
        // 不再嵌入冗余的 cells 数组（xlsx 已是无损真源），只保留对象层和合并信息
        data_sheets.push(json!({"name":name,"objects":objects,"merges":merges}));
    }
    // Brotli 压缩 xlsx 再 base64（体积减少 30-60%，解决大文件导出 HTML 卡顿）
    let xlsx_br = br_compress(&xlsx)
        .map(|c| b64_encode(&c))
        .unwrap_or(xlsx_b64.clone());
    let data = json!({
        "format":"unicell-html","version":2,"unidoc_type":"cell","app":"UniCell",
        "basename":basename,"xlsx_br":xlsx_br,"sheets":data_sheets,
    });
    // 防止 cell 内容里的 </script> 破坏脚本标签：转义 < 为 \u003c（仍是合法 JSON）
    let data_json = serde_json::to_string(&data)
        .map_err(|e| e.to_string())?
        .replace('<', "\\u003c");
    let html = format!(
        "<!doctype html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n<style>\
body{{font-family:'Segoe UI','Microsoft YaHei',sans-serif;margin:16px;color:#222;text-align:center;background:#f6f7f9}}\
h3{{margin:16px 0 4px;text-align:center}}\
.tbl-box{{position:relative;display:inline-block;text-align:left;margin:6px auto 26px;background:#fff;box-shadow:0 1px 6px rgba(0,0,0,.08)}}\
table{{border-collapse:collapse;table-layout:fixed}}\
td{{border:1px solid #d9d9d9;padding:2px 8px;font-size:13px;overflow:hidden;box-sizing:border-box;text-align:left;white-space:nowrap}}\
.obj-ov{{position:absolute;left:0;top:0}}\
.obj-ov>*{{position:absolute;box-sizing:border-box}}\
.obj-ov img{{object-fit:contain}}\
.obj-ov iframe{{border:1px solid #c3c9d2;border-radius:3px;background:#fff}}\
</style>
</head>
<body>
<div data-unicell=\"cell\">
{body}</div>
<script type=\"application/x-unicell+json\" id=\"unicell-data\">{data}</script>
</body>
</html>
",
        title = html_escape(&basename), body = body_html, data = data_json,
    );
    let fname = http_safe_attachment_name(&basename, "html");
    Ok(tiny_http::Response::from_data(html.into_bytes())
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Disposition"[..], fname.as_bytes()).unwrap(),
        ))
}

// 导入无损 HTML：提取内嵌 JSON → 校验 unidoc_type=cell → 解码 xlsx 还原 → 恢复对象层
fn api_import_html(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let html = std::str::from_utf8(body).map_err(|_| "html 非 UTF-8".to_string())?;
    let json_str =
        extract_unicell_script(html).ok_or("HTML 中未找到 UniCell 数据（非无损 HTML）")?;
    let data: Value = serde_json::from_str(&json_str).map_err(|e| format!("data json: {e}"))?;
    if data["unidoc_type"].as_str() != Some("cell") {
        return Err("不是 UniCell 的无损 HTML（unidoc_type 不是 cell）".into());
    }
    // 支持 v2（Brotli 压缩）和 v1（纯 base64）两种格式
    let xlsx = if let Some(br_b64) = data["xlsx_br"].as_str() {
        let compressed = b64_decode(br_b64)?;
        br_decompress(&compressed)?
    } else if let Some(raw_b64) = data["xlsx_b64"].as_str() {
        b64_decode(raw_b64)?
    } else {
        return Err("HTML 缺少 xlsx 无损载荷".into());
    };
    let names = load_xlsx_into_state(st, &xlsx)?;
    restore_objects_from_sheets(st, &data["sheets"], None);
    if let Some(bn) = data["basename"].as_str() {
        if !bn.is_empty() {
            st.file_name = bn.to_string();
        }
    }
    ok_json(json!({ "sheets": names }))
}

// 导入 udoc：解 UDOC3 → 校验 unidoc_type=cell → 载入内嵌 xlsx 还原 → 恢复对象层（media 内联）
fn api_import_udoc(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    let parts = decode_udoc3(body)?;
    let manifest = parts
        .get("manifest.json")
        .ok_or("udoc 缺少 manifest.json")?;
    let mv: Value = serde_json::from_slice(manifest).map_err(|e| format!("manifest json: {e}"))?;
    if mv["unidoc_type"].as_str() != Some("cell") {
        return Err("不是 UniCell 的 udoc（unidoc_type 不是 cell）".into());
    }
    let xlsx = parts
        .get("document/workbook.xlsx")
        .ok_or("udoc 缺少 workbook.xlsx（无损载荷缺失）")?;
    let names = load_xlsx_into_state(st, xlsx)?;
    if let Some(doc) = parts.get("document/document.json") {
        if let Ok(dv) = serde_json::from_slice::<Value>(doc) {
            restore_objects_from_sheets(st, &dv["sheets"], Some(&parts));
        }
    }
    if let Some(bn) = mv["basename"].as_str() {
        if !bn.is_empty() {
            st.file_name = bn.to_string();
        }
    }
    ok_json(json!({ "sheets": names }))
}

fn set_xml_usize_attr(xml: &mut String, name: &str, value: usize) {
    let needle = format!("{name}=\"");
    let Some(start) = xml.find(&needle).map(|i| i + needle.len()) else {
        if let Some(root_start) = xml.find("<sst") {
            if let Some(relative_end) = xml[root_start..].find('>') {
                let mut insert_at = root_start + relative_end;
                if xml.as_bytes().get(insert_at.wrapping_sub(1)) == Some(&b'/') {
                    insert_at -= 1;
                }
                xml.insert_str(insert_at, &format!(" {name}=\"{value}\""));
            }
        }
        return;
    };
    let Some(relative_end) = xml[start..].find('"') else {
        return;
    };
    xml.replace_range(start..start + relative_end, &value.to_string());
}

/// Reattaches the original OOXML shared-string rich runs after IronCalc export. The engine's
/// writer intentionally emits flattened strings, so without this pass import→export would lose
/// per-run color, weight, size, font, underline and other `<rPr>` properties.
fn inject_rich_shared_strings(
    xlsx: Vec<u8>,
    rich_xml: &std::collections::HashMap<(u32, i32, i32), String>,
) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Read, Write};
    if rich_xml.is_empty() {
        return Ok(xlsx);
    }
    let mut za =
        zip::read::ZipArchive::new(Cursor::new(xlsx)).map_err(|e| format!("rich zip read: {e}"))?;
    let mut entries: Vec<(String, Vec<u8>, zip::CompressionMethod, bool)> = Vec::new();
    for i in 0..za.len() {
        let mut file = za.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().to_string();
        let method = file.compression();
        let is_dir = file.is_dir();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        entries.push((name, bytes, method, is_dir));
    }
    drop(za);

    let entry_text = |name: &str| -> Option<String> {
        let bytes = &entries.iter().find(|(n, _, _, _)| n == name)?.1;
        String::from_utf8(bytes.clone()).ok()
    };
    let workbook_xml = entry_text("xl/workbook.xml").ok_or("rich export: workbook.xml missing")?;
    let rels_xml =
        entry_text("xl/_rels/workbook.xml.rels").ok_or("rich export: workbook rels missing")?;
    let workbook_doc =
        roxmltree::Document::parse(&workbook_xml).map_err(|e| format!("rich workbook xml: {e}"))?;
    let rels_doc =
        roxmltree::Document::parse(&rels_xml).map_err(|e| format!("rich rels xml: {e}"))?;
    let rel_targets: std::collections::HashMap<String, String> = rels_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("Relationship"))
        .filter_map(|n| {
            Some((
                n.attribute("Id")?.to_string(),
                n.attribute("Target")?.to_string(),
            ))
        })
        .collect();
    let sheet_paths: Vec<String> = workbook_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("sheet"))
        .filter_map(|sheet| {
            let id = sheet.attributes().find(|a| a.name() == "id")?.value();
            let target = rel_targets.get(id)?.replace('\\', "/");
            Some(if target.starts_with('/') {
                target.trim_start_matches('/').to_string()
            } else if target.starts_with("xl/") {
                target
            } else {
                format!("xl/{target}")
            })
        })
        .collect();

    let shared_index = entries
        .iter()
        .position(|(name, _, _, _)| name == "xl/sharedStrings.xml")
        .ok_or("rich export: sharedStrings.xml missing")?;
    let mut shared_xml = String::from_utf8(entries[shared_index].1.clone())
        .map_err(|e| format!("rich shared strings utf8: {e}"))?;
    let shared_doc = roxmltree::Document::parse(&shared_xml)
        .map_err(|e| format!("rich shared strings xml: {e}"))?;
    let base_index = shared_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("si"))
        .count();
    drop(shared_doc);

    let mut rich_cells: Vec<(&(u32, i32, i32), &String)> = rich_xml.iter().collect();
    rich_cells.sort_by_key(|(key, _)| **key);
    let mut updates: std::collections::HashMap<u32, Vec<(String, usize)>> =
        std::collections::HashMap::new();
    let mut appended = String::new();
    for (offset, (key, raw)) in rich_cells.iter().enumerate() {
        appended.push_str(raw);
        updates
            .entry(key.0)
            .or_default()
            .push((cell_ref(key.1, key.2), base_index + offset));
    }
    let close = shared_xml
        .rfind("</sst>")
        .ok_or("rich export: malformed sharedStrings")?;
    shared_xml.insert_str(close, &appended);
    entries[shared_index].1 = shared_xml.into_bytes();

    for (sheet, sheet_updates) in updates {
        let Some(path) = sheet_paths.get(sheet as usize) else {
            continue;
        };
        let Some(entry_index) = entries.iter().position(|(name, _, _, _)| name == path) else {
            continue;
        };
        let mut xml = String::from_utf8(entries[entry_index].1.clone())
            .map_err(|e| format!("rich sheet utf8: {e}"))?;
        let replacements: Vec<(std::ops::Range<usize>, String)> = {
            let doc =
                roxmltree::Document::parse(&xml).map_err(|e| format!("rich sheet xml: {e}"))?;
            sheet_updates
                .iter()
                .filter_map(|(reference, index)| {
                    let cell = doc.descendants().find(|n| {
                        n.is_element() && n.has_tag_name("c") && n.attribute("r") == Some(reference)
                    })?;
                    let value = cell
                        .children()
                        .find(|n| n.is_element() && n.has_tag_name("v"))?
                        .children()
                        .find(|n| n.is_text())?;
                    Some((value.range(), index.to_string()))
                })
                .collect()
        };
        let mut replacements = replacements;
        replacements.sort_by(|a, b| b.0.start.cmp(&a.0.start));
        for (range, value) in replacements {
            xml.replace_range(range, &value);
        }
        entries[entry_index].1 = xml.into_bytes();
    }

    // IronCalc historically wrote sst@count as the number of unique <si> entries. Excel defines
    // it as the total number of worksheet cells that reference the shared-string table, including
    // duplicates. Recompute both counters from the final package after all rich-index redirects.
    let shared_reference_count = sheet_paths.iter().try_fold(0usize, |count, path| {
        let Some((_, bytes, _, _)) = entries.iter().find(|(name, _, _, _)| name == path) else {
            return Ok::<usize, String>(count);
        };
        let xml = std::str::from_utf8(bytes)
            .map_err(|error| format!("rich sheet UTF-8 while counting shared strings: {error}"))?;
        let document = roxmltree::Document::parse(xml)
            .map_err(|error| format!("rich sheet XML while counting shared strings: {error}"))?;
        Ok(count
            + document
                .descendants()
                .filter(|node| {
                    node.is_element() && node.has_tag_name("c") && node.attribute("t") == Some("s")
                })
                .count())
    })?;
    let mut final_shared_xml = String::from_utf8(entries[shared_index].1.clone())
        .map_err(|error| format!("rich shared strings UTF-8 while counting: {error}"))?;
    let unique_count = roxmltree::Document::parse(&final_shared_xml)
        .map_err(|error| format!("rich shared strings XML while counting: {error}"))?
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name("si"))
        .count();
    set_xml_usize_attr(&mut final_shared_xml, "count", shared_reference_count);
    set_xml_usize_attr(&mut final_shared_xml, "uniqueCount", unique_count);
    entries[shared_index].1 = final_shared_xml.into_bytes();

    let mut zw = zip::write::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes, method, is_dir) in entries {
        let options = zip::write::FileOptions::default().compression_method(method);
        if is_dir {
            zw.add_directory(name, options).map_err(|e| e.to_string())?;
        } else {
            zw.start_file(name, options).map_err(|e| e.to_string())?;
            zw.write_all(&bytes).map_err(|e| e.to_string())?;
        }
    }
    Ok(zw.finish().map_err(|e| e.to_string())?.into_inner())
}

fn opc_relationship_owner(path: &str) -> Option<String> {
    if path == "_rels/.rels" {
        return Some(String::new());
    }
    let (prefix, file) = path.rsplit_once("/_rels/")?;
    let owner_file = file.strip_suffix(".rels")?;
    Some(format!("{prefix}/{owner_file}"))
}

fn opc_part_is_controlled(name: &str) -> bool {
    matches!(
        name,
        "[Content_Types].xml"
            | "_rels/.rels"
            | "docProps/app.xml"
            | "docProps/core.xml"
            | "xl/workbook.xml"
            | "xl/_rels/workbook.xml.rels"
            | "xl/styles.xml"
            | "xl/sharedStrings.xml"
            | "xl/calcChain.xml"
    ) || name.starts_with("xl/worksheets/")
        || name.starts_with("xl/charts/")
        || name.starts_with("xl/diagrams/")
        || name.starts_with("xl/media/")
        || (name.starts_with("xl/drawings/") && !name.ends_with(".vml"))
}

fn opc_part_is_preservable(name: &str) -> bool {
    !name.starts_with("_xmlsignatures/")
        && name != "origin.sigs"
        && !opc_part_is_controlled(name)
        && !name.ends_with(".rels")
}

fn opc_rel_should_survive(
    owner: &str,
    relationship: roxmltree::Node<'_, '_>,
    snapshot: &OpcPackageSnapshot,
) -> bool {
    if relationship.attribute("TargetMode") == Some("External") {
        return true;
    }
    let Some(target) = relationship.attribute("Target") else {
        return false;
    };
    let target = target.split('#').next().unwrap_or(target);
    let resolved = resolve_rel_path(&path_dir(owner), target);
    snapshot.parts.contains_key(&resolved) && opc_part_is_preservable(&resolved)
}

fn replace_relationship_id(fragment: &str, old: &str, new: &str) -> String {
    if old == new {
        return fragment.to_string();
    }
    let double = format!("Id=\"{old}\"");
    if fragment.contains(&double) {
        return fragment.replacen(&double, &format!("Id=\"{new}\""), 1);
    }
    let single = format!("Id='{old}'");
    fragment.replacen(&single, &format!("Id='{new}'"), 1)
}

fn remap_parent_relationship_ids(
    mut fragment: String,
    id_map: &std::collections::HashMap<String, String>,
) -> String {
    for (old, new) in id_map {
        if old == new {
            continue;
        }
        fragment = fragment.replace(&format!("r:id=\"{old}\""), &format!("r:id=\"{new}\""));
        fragment = fragment.replace(&format!("r:id='{old}'"), &format!("r:id='{new}'"));
    }
    fragment
}

fn remove_relationships_by_type_suffix(xml: &str, suffix: &str) -> Result<String, String> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|e| format!("relationships XML while removing {suffix}: {e}"))?;
    let mut ranges: Vec<std::ops::Range<usize>> = document
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
        .filter(|node| {
            node.attribute("Type")
                .map(|value| value.ends_with(suffix))
                .unwrap_or(false)
        })
        .map(|node| node.range())
        .collect();
    drop(document);
    ranges.sort_by_key(|range| range.start);
    let mut result = xml.to_string();
    for range in ranges.into_iter().rev() {
        result.replace_range(range, "");
    }
    Ok(result)
}

fn snapshot_workbook_theme_part(snapshot: &OpcPackageSnapshot) -> Option<String> {
    let rels = snapshot.parts.get("xl/_rels/workbook.xml.rels")?;
    let xml = std::str::from_utf8(rels).ok()?;
    let document = roxmltree::Document::parse(xml).ok()?;
    document
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
        .find(|node| {
            node.attribute("Type")
                .map(|value| value.ends_with("/theme"))
                .unwrap_or(false)
                && opc_rel_should_survive("xl/workbook.xml", *node, snapshot)
        })
        .and_then(|node| node.attribute("Target"))
        .map(|target| resolve_rel_path("xl", target.split('#').next().unwrap_or(target)))
}

fn merge_relationship_xml(
    generated: Option<&[u8]>,
    original: &[u8],
    owner: &str,
    snapshot: &OpcPackageSnapshot,
) -> Result<(Vec<u8>, std::collections::HashMap<String, String>), String> {
    let original_xml = std::str::from_utf8(original).map_err(|e| format!("OPC rels utf8: {e}"))?;
    let original_doc =
        roxmltree::Document::parse(original_xml).map_err(|e| format!("OPC rels XML: {e}"))?;
    let mut id_map = std::collections::HashMap::new();
    let mut generated_xml = match generated {
        Some(bytes) => std::str::from_utf8(bytes).map_err(|e| format!("generated rels utf8: {e}"))?.to_string(),
        None => "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_string(),
    };
    let replace_generated_theme = owner == "xl/workbook.xml"
        && original_doc
            .descendants()
            .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
            .any(|node| {
                node.attribute("Type")
                    .map(|value| value.ends_with("/theme"))
                    .unwrap_or(false)
                    && opc_rel_should_survive(owner, node, snapshot)
            });
    if replace_generated_theme {
        generated_xml = remove_relationships_by_type_suffix(&generated_xml, "/theme")?;
    }
    let (mut used_ids, mut existing): (
        std::collections::HashSet<String>,
        std::collections::HashMap<(String, String, String), String>,
    ) = {
        let generated_doc = roxmltree::Document::parse(&generated_xml)
            .map_err(|e| format!("generated rels XML: {e}"))?;
        let mut ids = std::collections::HashSet::new();
        let mut triples = std::collections::HashMap::new();
        for rel in generated_doc
            .descendants()
            .filter(|n| n.is_element() && n.has_tag_name("Relationship"))
        {
            let id = rel.attribute("Id").unwrap_or("").to_string();
            ids.insert(id.clone());
            triples.insert(
                (
                    rel.attribute("Type").unwrap_or("").to_string(),
                    rel.attribute("Target").unwrap_or("").to_string(),
                    rel.attribute("TargetMode").unwrap_or("").to_string(),
                ),
                id,
            );
        }
        (ids, triples)
    };
    let mut appended = String::new();
    let mut next_id = 1usize;
    let mut kept_original_theme = false;
    for rel in original_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("Relationship"))
    {
        if !opc_rel_should_survive(owner, rel, snapshot) {
            continue;
        }
        if owner == "xl/workbook.xml"
            && rel
                .attribute("Type")
                .map(|value| value.ends_with("/theme"))
                .unwrap_or(false)
        {
            if kept_original_theme {
                continue;
            }
            kept_original_theme = true;
        }
        let old_id = rel.attribute("Id").unwrap_or("").to_string();
        let key = (
            rel.attribute("Type").unwrap_or("").to_string(),
            rel.attribute("Target").unwrap_or("").to_string(),
            rel.attribute("TargetMode").unwrap_or("").to_string(),
        );
        if let Some(existing_id) = existing.get(&key) {
            id_map.insert(old_id, existing_id.clone());
            continue;
        }
        let mut new_id = old_id.clone();
        if new_id.is_empty() || used_ids.contains(&new_id) {
            loop {
                let candidate = format!("rIdPreserved{next_id}");
                next_id += 1;
                if !used_ids.contains(&candidate) {
                    new_id = candidate;
                    break;
                }
            }
        }
        let fragment = &original_xml[rel.range()];
        appended.push_str(&replace_relationship_id(fragment, &old_id, &new_id));
        used_ids.insert(new_id.clone());
        existing.insert(key, new_id.clone());
        id_map.insert(old_id, new_id);
    }
    if !appended.is_empty() {
        let close = generated_xml
            .rfind("</Relationships>")
            .ok_or("malformed generated relationships")?;
        generated_xml.insert_str(close, &appended);
    }
    Ok((generated_xml.into_bytes(), id_map))
}

fn merge_root_namespace_declarations(generated: &mut String, original: &str) {
    fn root_open_tag(xml: &str) -> Option<&str> {
        let doc = roxmltree::Document::parse(xml).ok()?;
        let root = doc.root_element();
        let start = root.range().start;
        let end = start + xml[start..].find('>')? + 1;
        Some(&xml[start..end])
    }
    let Some(original_tag) = root_open_tag(original) else {
        return;
    };
    let Some(generated_tag) = root_open_tag(generated).map(str::to_string) else {
        return;
    };
    let mut declarations = Vec::new();
    let mut offset = 0usize;
    while let Some(relative) = original_tag[offset..].find(" xmlns") {
        let start = offset + relative + 1;
        let Some(eq_relative) = original_tag[start..].find('=') else {
            break;
        };
        let eq = start + eq_relative;
        let name = original_tag[start..eq].trim();
        if !(name == "xmlns" || name.starts_with("xmlns:")) {
            offset = eq + 1;
            continue;
        }
        let quote_at = eq + 1 + original_tag[eq + 1..].find(['\"', '\'']).unwrap_or(0);
        let Some(quote) = original_tag.as_bytes().get(quote_at).copied() else {
            break;
        };
        let Some(end_relative) = original_tag[quote_at + 1..].find(quote as char) else {
            break;
        };
        let end = quote_at + 1 + end_relative + 1;
        if !generated_tag.contains(&format!("{name}=")) {
            declarations.push(original_tag[start..end].to_string());
        }
        offset = end;
    }
    if !declarations.is_empty() {
        let Ok(doc) = roxmltree::Document::parse(generated) else {
            return;
        };
        let root_start = doc.root_element().range().start;
        let Some(end) = generated[root_start..].find('>').map(|v| root_start + v) else {
            return;
        };
        generated.insert_str(end, &format!(" {}", declarations.join(" ")));
    }
    let Some(original_ignorable) = xml_attr(original_tag, "mc:Ignorable") else {
        return;
    };
    let Some(current_tag) = root_open_tag(generated).map(str::to_string) else {
        return;
    };
    let mut tokens: Vec<String> = xml_attr(&current_tag, "mc:Ignorable")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    for token in original_ignorable.split_whitespace() {
        if !tokens.iter().any(|current| current == token) {
            tokens.push(token.to_string());
        }
    }
    let updated = set_start_tag_attribute(&current_tag, "mc:Ignorable", &tokens.join(" "));
    if updated != current_tag {
        if let Some(start) = generated.find(&current_tag) {
            generated.replace_range(start..start + current_tag.len(), &updated);
        }
    }
}

fn merge_top_level_blocks(
    generated: &mut String,
    original: &str,
    ordered_names: &[&str],
    preserve_names: &std::collections::HashSet<&str>,
    id_map: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    merge_root_namespace_declarations(generated, original);
    let original_doc =
        roxmltree::Document::parse(original).map_err(|e| format!("original parent XML: {e}"))?;
    let original_root = original_doc.root_element();
    let order: std::collections::HashMap<&str, usize> = ordered_names
        .iter()
        .enumerate()
        .map(|(i, name)| (*name, i))
        .collect();
    let fragments: Vec<(String, String)> = original_root
        .children()
        .filter(|n| n.is_element() && preserve_names.contains(n.tag_name().name()))
        .map(|n| {
            (
                n.tag_name().name().to_string(),
                original[n.range()].to_string(),
            )
        })
        .collect();
    for (name, raw) in fragments {
        let current_doc = roxmltree::Document::parse(generated)
            .map_err(|e| format!("generated parent XML: {e}"))?;
        let root = current_doc.root_element();
        if root
            .children()
            .any(|n| n.is_element() && n.tag_name().name() == name)
        {
            continue;
        }
        let wanted_order = order.get(name.as_str()).copied().unwrap_or(usize::MAX - 1);
        let insert_at = root
            .children()
            .filter(|n| n.is_element())
            .find(|n| {
                order
                    .get(n.tag_name().name())
                    .copied()
                    .unwrap_or(usize::MAX)
                    > wanted_order
            })
            .map(|n| n.range().start)
            .unwrap_or_else(|| {
                let closing = format!("</{}>", root.tag_name().name());
                generated.rfind(&closing).unwrap_or(generated.len())
            });
        let fragment = remap_parent_relationship_ids(raw, id_map);
        drop(current_doc);
        generated.insert_str(insert_at, &fragment);
    }
    Ok(())
}

fn replace_existing_top_level_block(
    generated: &mut String,
    original: &str,
    local: &str,
) -> Result<(), String> {
    let original_document = roxmltree::Document::parse(original)
        .map_err(|error| format!("original parent XML: {error}"))?;
    let Some(original_node) = original_document
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == local)
    else {
        return Ok(());
    };
    let fragment = original[original_node.range()].to_string();
    let generated_document = roxmltree::Document::parse(generated)
        .map_err(|error| format!("generated parent XML: {error}"))?;
    let range = generated_document
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == local)
        .map(|node| node.range());
    drop(generated_document);
    if let Some(range) = range {
        generated.replace_range(range, &fragment);
    }
    Ok(())
}

fn worksheet_child_order(name: &str) -> usize {
    const ORDER: &[&str] = &[
        "sheetPr",
        "dimension",
        "sheetViews",
        "sheetFormatPr",
        "cols",
        "sheetData",
        "sheetCalcPr",
        "sheetProtection",
        "protectedRanges",
        "scenarios",
        "autoFilter",
        "sortState",
        "dataConsolidate",
        "customSheetViews",
        "mergeCells",
        "phoneticPr",
        "conditionalFormatting",
        "dataValidations",
        "hyperlinks",
        "printOptions",
        "pageMargins",
        "pageSetup",
        "headerFooter",
        "rowBreaks",
        "colBreaks",
        "customProperties",
        "cellWatches",
        "ignoredErrors",
        "smartTags",
        "drawing",
        "legacyDrawing",
        "legacyDrawingHF",
        "picture",
        "oleObjects",
        "controls",
        "webPublishItems",
        "tableParts",
        "pivotTableParts",
        "extLst",
    ];
    ORDER
        .iter()
        .position(|item| *item == name)
        .unwrap_or(usize::MAX - 1)
}

fn insert_worksheet_fragment(xml: &mut String, local: &str, fragment: &str) -> Result<(), String> {
    let document =
        roxmltree::Document::parse(xml).map_err(|e| format!("worksheet insert XML: {e}"))?;
    let root = document.root_element();
    let wanted = worksheet_child_order(local);
    let insert_at = root
        .children()
        .filter(|node| node.is_element())
        .find(|node| worksheet_child_order(node.tag_name().name()) > wanted)
        .map(|node| node.range().start)
        .unwrap_or_else(|| {
            xml.rfind(&format!("</{}>", root.tag_name().name()))
                .unwrap_or(xml.len())
        });
    drop(document);
    xml.insert_str(insert_at, fragment);
    Ok(())
}

fn replace_top_level_from_original(
    generated: &mut String,
    original: &str,
    local: &str,
) -> Result<(), String> {
    merge_root_namespace_declarations(generated, original);
    let original_document = roxmltree::Document::parse(original)
        .map_err(|e| format!("original worksheet feature XML: {e}"))?;
    let fragments: Vec<String> = original_document
        .root_element()
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == local)
        .map(|node| original[node.range()].to_string())
        .collect();
    let generated_document = roxmltree::Document::parse(generated)
        .map_err(|e| format!("generated worksheet feature XML: {e}"))?;
    let mut ranges: Vec<std::ops::Range<usize>> = generated_document
        .root_element()
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == local)
        .map(|node| node.range())
        .collect();
    drop(generated_document);
    ranges.sort_by(|left, right| right.start.cmp(&left.start));
    for range in ranges {
        generated.replace_range(range, "");
    }
    if !fragments.is_empty() {
        insert_worksheet_fragment(generated, local, &fragments.join(""))?;
    }
    Ok(())
}

fn xml_boolean(value: Option<&str>) -> bool {
    value
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "on"))
        .unwrap_or(false)
}

fn direct_child_text(node: roxmltree::Node<'_, '_>, local: &str) -> Option<String> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == local)
        .map(|child| child.text().unwrap_or("").to_string())
}

fn data_validation_rule_id(sheet: u32, index: usize, raw: &str) -> String {
    let digest = sha2::Sha256::digest(raw.as_bytes());
    let suffix = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("dv-{sheet}-{index}-{suffix}")
}

fn parse_data_validation_sheet(sheet: u32, worksheet_xml: &str) -> DataValidationSheet {
    let Ok(document) = roxmltree::Document::parse(worksheet_xml) else {
        return DataValidationSheet::default();
    };
    let Some(container) = document
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "dataValidations")
    else {
        return DataValidationSheet::default();
    };
    let container_raw = &worksheet_xml[container.range()];
    let mut container_start_tag = container_raw
        .find('>')
        .map(|end| container_raw[..=end].to_string())
        .unwrap_or_else(|| "<dataValidations count=\"0\">".to_string());
    if container_start_tag.ends_with("/>") {
        container_start_tag.replace_range(container_start_tag.len() - 2.., ">");
    }
    let mut rules = Vec::new();
    for (index, rule) in container
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "dataValidation")
        .enumerate()
    {
        let raw = worksheet_xml[rule.range()].to_string();
        rules.push(DataValidationRule {
            id: data_validation_rule_id(sheet, index, &raw),
            sqref: rule.attribute("sqref").unwrap_or("").to_string(),
            validation_type: rule.attribute("type").unwrap_or("any").to_string(),
            operator: rule.attribute("operator").map(str::to_string),
            allow_blank: xml_boolean(rule.attribute("allowBlank")),
            in_cell_dropdown: !xml_boolean(rule.attribute("showDropDown")),
            show_input_message: xml_boolean(rule.attribute("showInputMessage")),
            show_error_message: xml_boolean(rule.attribute("showErrorMessage")),
            error_style: rule.attribute("errorStyle").map(str::to_string),
            ime_mode: rule.attribute("imeMode").map(str::to_string),
            prompt_title: rule.attribute("promptTitle").map(str::to_string),
            prompt: rule.attribute("prompt").map(str::to_string),
            error_title: rule.attribute("errorTitle").map(str::to_string),
            error: rule.attribute("error").map(str::to_string),
            formula1: direct_child_text(rule, "formula1"),
            formula2: direct_child_text(rule, "formula2"),
            raw_xml: Some(raw),
        });
    }
    DataValidationSheet {
        container_start_tag,
        rules,
    }
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value.and_then(|value| if value.is_empty() { None } else { Some(value) })
}

fn normalize_data_validation_rule(
    mut rule: DataValidationRule,
) -> Result<DataValidationRule, String> {
    const TYPES: &[&str] = &[
        "any",
        "none",
        "whole",
        "decimal",
        "list",
        "date",
        "time",
        "textLength",
        "custom",
    ];
    const OPERATORS: &[&str] = &[
        "between",
        "notBetween",
        "equal",
        "notEqual",
        "greaterThan",
        "lessThan",
        "greaterThanOrEqual",
        "lessThanOrEqual",
    ];
    const ERROR_STYLES: &[&str] = &["stop", "warning", "information"];
    if !TYPES.contains(&rule.validation_type.as_str()) {
        return Err(format!(
            "unsupported data validation type: {}",
            rule.validation_type
        ));
    }
    rule.validation_type = match rule.validation_type.as_str() {
        "none" => "any".to_string(),
        _ => rule.validation_type,
    };
    rule.sqref = rule.sqref.split_whitespace().collect::<Vec<_>>().join(" ");
    if rule.sqref.is_empty()
        || rule.sqref.len() > 32_767
        || !rule
            .sqref
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '$' | ':' | ' '))
    {
        return Err("invalid data validation sqref".to_string());
    }
    if let Some(operator) = rule.operator.as_deref() {
        if !OPERATORS.contains(&operator) {
            return Err(format!("unsupported data validation operator: {operator}"));
        }
    }
    if let Some(style) = rule.error_style.as_deref() {
        if !ERROR_STYLES.contains(&style) {
            return Err(format!("unsupported data validation error style: {style}"));
        }
    }
    rule.operator = normalize_optional_text(rule.operator);
    rule.error_style = normalize_optional_text(rule.error_style);
    rule.ime_mode = normalize_optional_text(rule.ime_mode);
    rule.prompt_title = normalize_optional_text(rule.prompt_title);
    rule.prompt = normalize_optional_text(rule.prompt);
    rule.error_title = normalize_optional_text(rule.error_title);
    rule.error = normalize_optional_text(rule.error);
    rule.formula1 = normalize_optional_text(rule.formula1)
        .map(|formula| formula.strip_prefix('=').unwrap_or(&formula).to_string());
    rule.formula2 = normalize_optional_text(rule.formula2)
        .map(|formula| formula.strip_prefix('=').unwrap_or(&formula).to_string());
    if rule
        .prompt_title
        .as_ref()
        .map(|s| s.chars().count())
        .unwrap_or(0)
        > 32
        || rule
            .error_title
            .as_ref()
            .map(|s| s.chars().count())
            .unwrap_or(0)
            > 32
    {
        return Err("data validation titles are limited to 32 characters".to_string());
    }
    if rule.prompt.as_ref().map(|s| s.chars().count()).unwrap_or(0) > 255
        || rule.error.as_ref().map(|s| s.chars().count()).unwrap_or(0) > 255
    {
        return Err("data validation messages are limited to 255 characters".to_string());
    }
    Ok(rule)
}

fn set_or_remove_start_tag_attribute(
    mut fragment: String,
    name: &str,
    value: Option<&str>,
) -> String {
    if let Some(value) = value {
        let escaped = html_escape(value).replace('\'', "&apos;");
        fragment = set_start_tag_attribute(&fragment, name, &escaped);
    } else {
        fragment = remove_start_tag_attribute(&fragment, name);
    }
    fragment
}

fn xml_fragment_namespace_prefixes(fragment: &str) -> Vec<String> {
    let bytes = fragment.as_bytes();
    let mut prefixes = std::collections::BTreeSet::new();
    for (colon, byte) in bytes.iter().enumerate() {
        if *byte != b':' {
            continue;
        }
        let mut start = colon;
        while start > 0
            && (bytes[start - 1].is_ascii_alphanumeric()
                || matches!(bytes[start - 1], b'_' | b'-' | b'.'))
        {
            start -= 1;
        }
        let prefix = &fragment[start..colon];
        if prefix.is_empty()
            || !prefix
                .as_bytes()
                .first()
                .map(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                .unwrap_or(false)
            || matches!(prefix, "xml" | "xmlns")
        {
            continue;
        }
        prefixes.insert(prefix.to_string());
    }
    prefixes.into_iter().collect()
}

fn xml_fragment_direct_child_range(fragment: &str, local: &str) -> Option<std::ops::Range<usize>> {
    // A rule fragment usually inherits its namespace declarations from the worksheet. Wrap it
    // with temporary declarations so roxmltree can still identify direct children by local name.
    let declarations = xml_fragment_namespace_prefixes(fragment)
        .into_iter()
        .map(|prefix| format!(" xmlns:{prefix}=\"urn:unicell:temporary:{prefix}\""))
        .collect::<String>();
    let opening = format!("<unicell-dv-root{declarations}>");
    let wrapped = format!("{opening}{fragment}</unicell-dv-root>");
    let document = roxmltree::Document::parse(&wrapped).ok()?;
    let rule = document
        .root_element()
        .children()
        .find(|node| node.is_element())?;
    let child = rule
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == local)?;
    let range = child.range();
    (range.start >= opening.len() && range.end >= opening.len())
        .then(|| range.start - opening.len()..range.end - opening.len())
}

fn xml_opening_qualified_name(fragment: &str) -> Option<String> {
    let start = fragment.find('<')? + 1;
    let bytes = fragment.as_bytes();
    let mut end = start;
    while bytes.get(end).map_or(false, |byte| {
        byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.' | b':')
    }) {
        end += 1;
    }
    (end > start).then(|| fragment[start..end].to_string())
}

fn xml_qualified_name_with_local(reference: &str, local: &str) -> String {
    reference
        .rsplit_once(':')
        .map(|(prefix, _)| format!("{prefix}:{local}"))
        .unwrap_or_else(|| local.to_string())
}

fn xml_start_tag_end(fragment: &str) -> Option<usize> {
    let mut quote = None;
    for (index, ch) in fragment.char_indices() {
        match quote {
            Some(current) if ch == current => quote = None,
            Some(_) => {}
            None if matches!(ch, '\'' | '"') => quote = Some(ch),
            None if ch == '>' => return Some(index),
            None => {}
        }
    }
    None
}

fn xml_self_closing_slash(start_tag: &str) -> Option<usize> {
    let tag_end = xml_start_tag_end(start_tag)?;
    start_tag[..tag_end]
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_whitespace())
        .and_then(|(index, ch)| (ch == '/').then_some(index))
}

fn replace_data_validation_formula(
    mut fragment: String,
    local: &str,
    value: Option<&str>,
) -> String {
    if let Some(range) = xml_fragment_direct_child_range(&fragment, local) {
        let Some(value) = value else {
            fragment.replace_range(range, "");
            return fragment;
        };
        let current = &fragment[range.clone()];
        let Some(qualified_name) = xml_opening_qualified_name(current) else {
            return fragment;
        };
        let Some(tag_end) = xml_start_tag_end(current) else {
            return fragment;
        };
        let escaped = html_escape(value);
        let replacement = if let Some(slash) = xml_self_closing_slash(&current[..=tag_end]) {
            let mut opening = current[..=tag_end].to_string();
            opening.remove(slash);
            format!("{opening}{escaped}</{qualified_name}>")
        } else {
            let closing = format!("</{qualified_name}>");
            let Some(close_start) = current.rfind(&closing) else {
                return fragment;
            };
            let mut replacement = current.to_string();
            replacement.replace_range(tag_end + 1..close_start, &escaped);
            replacement
        };
        fragment.replace_range(range, &replacement);
        return fragment;
    }
    let Some(value) = value else {
        return fragment;
    };
    let Some(root_name) = xml_opening_qualified_name(&fragment) else {
        return fragment;
    };
    let formula_name = xml_qualified_name_with_local(&root_name, local);
    let replacement = format!("<{formula_name}>{}</{formula_name}>", html_escape(value));
    let Some(tag_end) = xml_start_tag_end(&fragment) else {
        return fragment;
    };
    if let Some(slash) = xml_self_closing_slash(&fragment[..=tag_end]) {
        fragment.remove(slash);
        fragment.push_str(&replacement);
        fragment.push_str(&format!("</{root_name}>"));
        return fragment;
    }
    let insert_at = if local == "formula1" {
        xml_fragment_direct_child_range(&fragment, "formula2").map(|range| range.start)
    } else {
        None
    }
    .or_else(|| xml_fragment_direct_child_range(&fragment, "extLst").map(|range| range.start))
    .or_else(|| fragment.rfind(&format!("</{root_name}>")));
    let Some(insert_at) = insert_at else {
        return fragment;
    };
    fragment.insert_str(insert_at, &replacement);
    fragment
}

fn serialize_data_validation_rule(rule: &DataValidationRule, element_name: &str) -> String {
    let mut fragment = rule
        .raw_xml
        .clone()
        .unwrap_or_else(|| format!("<{element_name}/>"));
    let validation_type = if rule.validation_type == "any" {
        None
    } else {
        Some(rule.validation_type.as_str())
    };
    fragment = set_or_remove_start_tag_attribute(fragment, "type", validation_type);
    fragment = set_or_remove_start_tag_attribute(fragment, "operator", rule.operator.as_deref());
    fragment = set_or_remove_start_tag_attribute(fragment, "sqref", Some(&rule.sqref));
    fragment = set_or_remove_start_tag_attribute(
        fragment,
        "allowBlank",
        Some(if rule.allow_blank { "1" } else { "0" }),
    );
    fragment = set_or_remove_start_tag_attribute(
        fragment,
        "showDropDown",
        Some(if rule.in_cell_dropdown { "0" } else { "1" }),
    );
    fragment = set_or_remove_start_tag_attribute(
        fragment,
        "showInputMessage",
        Some(if rule.show_input_message { "1" } else { "0" }),
    );
    fragment = set_or_remove_start_tag_attribute(
        fragment,
        "showErrorMessage",
        Some(if rule.show_error_message { "1" } else { "0" }),
    );
    fragment =
        set_or_remove_start_tag_attribute(fragment, "errorStyle", rule.error_style.as_deref());
    fragment = set_or_remove_start_tag_attribute(fragment, "imeMode", rule.ime_mode.as_deref());
    fragment =
        set_or_remove_start_tag_attribute(fragment, "promptTitle", rule.prompt_title.as_deref());
    fragment = set_or_remove_start_tag_attribute(fragment, "prompt", rule.prompt.as_deref());
    fragment =
        set_or_remove_start_tag_attribute(fragment, "errorTitle", rule.error_title.as_deref());
    fragment = set_or_remove_start_tag_attribute(fragment, "error", rule.error.as_deref());
    fragment = replace_data_validation_formula(fragment, "formula1", rule.formula1.as_deref());
    replace_data_validation_formula(fragment, "formula2", rule.formula2.as_deref())
}

fn apply_data_validation_sheet(
    generated: &mut String,
    sheet: &DataValidationSheet,
) -> Result<(), String> {
    let document = roxmltree::Document::parse(generated)
        .map_err(|error| format!("generated data validation XML: {error}"))?;
    let mut ranges: Vec<std::ops::Range<usize>> = document
        .root_element()
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "dataValidations")
        .map(|node| node.range())
        .collect();
    drop(document);
    ranges.sort_by(|left, right| right.start.cmp(&left.start));
    for range in ranges {
        generated.replace_range(range, "");
    }
    if sheet.rules.is_empty() {
        return Ok(());
    }
    let mut start_tag = if sheet.container_start_tag.is_empty() {
        "<dataValidations>".to_string()
    } else {
        sheet.container_start_tag.clone()
    };
    start_tag = set_start_tag_attribute(&start_tag, "count", &sheet.rules.len().to_string());
    if start_tag.ends_with("/>") {
        start_tag.replace_range(start_tag.len() - 2.., ">");
    }
    let container_name =
        xml_opening_qualified_name(&start_tag).unwrap_or_else(|| "dataValidations".to_string());
    let rule_name = xml_qualified_name_with_local(&container_name, "dataValidation");
    let rules = sheet
        .rules
        .iter()
        .map(|rule| serialize_data_validation_rule(rule, &rule_name))
        .collect::<String>();
    insert_worksheet_fragment(
        generated,
        "dataValidations",
        &format!("{start_tag}{rules}</{container_name}>"),
    )
}

#[derive(Debug, Clone, Copy)]
struct DataValidationReference {
    row: Option<i32>,
    column: Option<i32>,
    row_absolute: bool,
    column_absolute: bool,
}

fn column_number(value: &str) -> Option<i32> {
    let mut column = 0i64;
    for ch in value.chars() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        column = column
            .checked_mul(26)?
            .checked_add((ch.to_ascii_uppercase() as i64) - ('A' as i64) + 1)?;
    }
    (1..=MAX_COLS as i64)
        .contains(&column)
        .then_some(column as i32)
}

fn parse_data_validation_reference(value: &str) -> Option<DataValidationReference> {
    if value.is_empty() {
        return None;
    }
    let bytes = value.as_bytes();
    let mut index = 0usize;
    let first_absolute = bytes.get(index) == Some(&b'$');
    if first_absolute {
        index += 1;
    }
    let letters_start = index;
    while bytes
        .get(index)
        .map(|byte| byte.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        index += 1;
    }
    if index > letters_start {
        let column = column_number(&value[letters_start..index])?;
        let row_absolute = bytes.get(index) == Some(&b'$');
        if row_absolute {
            index += 1;
        }
        let row_start = index;
        while bytes
            .get(index)
            .map(|byte| byte.is_ascii_digit())
            .unwrap_or(false)
        {
            index += 1;
        }
        if index != bytes.len() {
            return None;
        }
        let row = if index == row_start {
            None
        } else {
            let row = value[row_start..index].parse::<i32>().ok()?;
            (1..=MAX_ROWS).contains(&row).then_some(row)
        };
        return Some(DataValidationReference {
            row,
            column: Some(column),
            row_absolute,
            column_absolute: first_absolute,
        });
    }
    let row_start = index;
    while bytes
        .get(index)
        .map(|byte| byte.is_ascii_digit())
        .unwrap_or(false)
    {
        index += 1;
    }
    if index != bytes.len() || index == row_start {
        return None;
    }
    let row = value[row_start..index].parse::<i32>().ok()?;
    if !(1..=MAX_ROWS).contains(&row) {
        return None;
    }
    Some(DataValidationReference {
        row: Some(row),
        column: None,
        row_absolute: first_absolute,
        column_absolute: false,
    })
}

fn format_data_validation_reference(reference: DataValidationReference) -> String {
    let mut result = String::new();
    if let Some(column) = reference.column {
        if reference.column_absolute {
            result.push('$');
        }
        result.push_str(&num_to_col(column as i64));
    }
    if let Some(row) = reference.row {
        if reference.row_absolute {
            result.push('$');
        }
        result.push_str(&row.to_string());
    }
    result
}

fn transform_validation_interval(
    start: i32,
    end: i32,
    at: i32,
    count: i32,
    delete: bool,
) -> Option<(i32, i32)> {
    if !delete {
        return Some(if at <= start {
            (start + count, end + count)
        } else if at <= end {
            (start, end + count)
        } else {
            (start, end)
        });
    }
    let delete_end = at.saturating_add(count - 1);
    if end < at {
        return Some((start, end));
    }
    if start > delete_end {
        return Some((start - count, end - count));
    }
    let before = (at - start).max(0);
    let after = (end - delete_end).max(0);
    let length = before + after;
    if length <= 0 {
        return None;
    }
    let new_start = if before > 0 { start } else { at };
    Some((new_start, new_start + length - 1))
}

fn transform_validation_range(
    first: DataValidationReference,
    second: DataValidationReference,
    rows: bool,
    at: i32,
    count: i32,
    delete: bool,
) -> Option<(DataValidationReference, DataValidationReference)> {
    let (start, end) = if rows {
        (first.row, second.row)
    } else {
        (first.column, second.column)
    };
    let (Some(start), Some(end)) = (start, end) else {
        return Some((first, second));
    };
    let (new_start, new_end) = transform_validation_interval(start, end, at, count, delete)?;
    let mut first = first;
    let mut second = second;
    if rows {
        first.row = Some(new_start);
        second.row = Some(new_end);
    } else {
        first.column = Some(new_start);
        second.column = Some(new_end);
    }
    Some((first, second))
}

fn transform_data_validation_sqref(
    sqref: &str,
    rows: bool,
    at: i32,
    count: i32,
    delete: bool,
) -> String {
    let mut transformed = Vec::new();
    for token in sqref.split_whitespace() {
        let (left, right, ranged) = if let Some((left, right)) = token.split_once(':') {
            (left, right, true)
        } else {
            (token, token, false)
        };
        let Some(first) = parse_data_validation_reference(left) else {
            transformed.push(token.to_string());
            continue;
        };
        let Some(second) = parse_data_validation_reference(right) else {
            transformed.push(token.to_string());
            continue;
        };
        let Some((first, second)) =
            transform_validation_range(first, second, rows, at, count, delete)
        else {
            continue;
        };
        let left = format_data_validation_reference(first);
        let right = format_data_validation_reference(second);
        transformed.push(if ranged || left != right {
            format!("{left}:{right}")
        } else {
            left
        });
    }
    transformed.join(" ")
}

fn formula_qualifier_before(formula: &str, start: usize) -> Option<String> {
    let bytes = formula.as_bytes();
    if start == 0 || bytes.get(start - 1) != Some(&b'!') {
        return None;
    }
    let bang = start - 1;
    if bang > 0 && bytes[bang - 1] == b'\'' {
        let mut cursor = bang - 1;
        while cursor > 0 {
            cursor -= 1;
            if bytes[cursor] == b'\'' {
                return std::str::from_utf8(&bytes[cursor + 1..bang - 1])
                    .ok()
                    .map(|value| value.replace("''", "'"));
            }
        }
        return Some(String::new());
    }
    let mut cursor = bang;
    while cursor > 0 {
        let byte = bytes[cursor - 1];
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.') || byte >= 0x80 {
            cursor -= 1;
        } else {
            break;
        }
    }
    std::str::from_utf8(&bytes[cursor..bang])
        .ok()
        .map(str::to_string)
}

fn formula_reference_applies(
    formula: &str,
    start: usize,
    target_sheet: &str,
    unqualified_targets_sheet: bool,
) -> bool {
    formula_qualifier_before(formula, start)
        .map(|qualifier| qualifier.eq_ignore_ascii_case(target_sheet))
        .unwrap_or(unqualified_targets_sheet)
}

fn parse_formula_cell_reference(
    formula: &str,
    start: usize,
) -> Option<(usize, DataValidationReference)> {
    let bytes = formula.as_bytes();
    let mut end = start;
    if bytes.get(end) == Some(&b'$') {
        end += 1;
    }
    let column_start = end;
    while bytes
        .get(end)
        .map(|byte| byte.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        end += 1;
    }
    if end == column_start || end - column_start > 3 {
        return None;
    }
    if bytes.get(end) == Some(&b'$') {
        end += 1;
    }
    let row_start = end;
    while bytes
        .get(end)
        .map(|byte| byte.is_ascii_digit())
        .unwrap_or(false)
    {
        end += 1;
    }
    if end == row_start
        || (start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_'))
        || bytes
            .get(end)
            .map(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            .unwrap_or(false)
    {
        return None;
    }
    parse_data_validation_reference(&formula[start..end]).map(|reference| (end, reference))
}

fn transform_data_validation_formula(
    formula: &str,
    rows: bool,
    at: i32,
    count: i32,
    delete: bool,
    target_sheet: &str,
    unqualified_targets_sheet: bool,
) -> String {
    let bytes = formula.as_bytes();
    let mut result = String::with_capacity(formula.len());
    let mut index = 0usize;
    let mut in_string = false;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            result.push('"');
            if in_string && bytes.get(index + 1) == Some(&b'"') {
                result.push('"');
                index += 2;
                continue;
            }
            in_string = !in_string;
            index += 1;
            continue;
        }
        if in_string {
            let ch = formula[index..].chars().next().unwrap();
            result.push(ch);
            index += ch.len_utf8();
            continue;
        }
        let Some((first_end, first)) = parse_formula_cell_reference(formula, index) else {
            let ch = formula[index..].chars().next().unwrap();
            result.push(ch);
            index += ch.len_utf8();
            continue;
        };
        if !formula_reference_applies(formula, index, target_sheet, unqualified_targets_sheet) {
            result.push_str(&formula[index..first_end]);
            index = first_end;
            continue;
        }
        let (end, second, ranged) = if bytes.get(first_end) == Some(&b':') {
            if let Some((second_end, second)) = parse_formula_cell_reference(formula, first_end + 1)
            {
                (second_end, second, true)
            } else {
                (first_end, first, false)
            }
        } else {
            (first_end, first, false)
        };
        if let Some((first, second)) =
            transform_validation_range(first, second, rows, at, count, delete)
        {
            result.push_str(&format_data_validation_reference(first));
            if ranged {
                result.push(':');
                result.push_str(&format_data_validation_reference(second));
            }
        } else {
            result.push_str("#REF!");
        }
        index = end;
    }
    result
}

fn transform_data_validations_for_structure(
    st: &mut AppState,
    edited_sheet: u32,
    rows: bool,
    at: i32,
    count: i32,
    delete: bool,
) {
    let target_sheet = st
        .model
        .get_model()
        .workbook
        .worksheets
        .get(edited_sheet as usize)
        .map(|worksheet| worksheet.name.clone())
        .unwrap_or_default();
    let mut dirty = Vec::new();
    for (rule_sheet, transport) in &mut st.worksheet_features.data_validations {
        let mut changed = false;
        if *rule_sheet == edited_sheet {
            for rule in &mut transport.rules {
                let sqref = transform_data_validation_sqref(&rule.sqref, rows, at, count, delete);
                if sqref != rule.sqref {
                    rule.sqref = sqref;
                    changed = true;
                }
            }
            let before = transport.rules.len();
            transport.rules.retain(|rule| !rule.sqref.is_empty());
            changed |= transport.rules.len() != before;
        }
        for rule in &mut transport.rules {
            for formula in [&mut rule.formula1, &mut rule.formula2] {
                let Some(current) = formula.as_ref() else {
                    continue;
                };
                let transformed = transform_data_validation_formula(
                    current,
                    rows,
                    at,
                    count,
                    delete,
                    &target_sheet,
                    *rule_sheet == edited_sheet,
                );
                if transformed != *current {
                    *formula = Some(transformed);
                    changed = true;
                }
            }
        }
        if changed {
            dirty.push(*rule_sheet);
        }
    }
    st.worksheet_features.data_validation_dirty.extend(dirty);
}

fn data_validation_index_after_move(index: u32, from: u32, to: u32) -> u32 {
    if index == from {
        to
    } else if from < to && index > from && index <= to {
        index - 1
    } else if to < from && index >= to && index < from {
        index + 1
    } else {
        index
    }
}

fn remap_data_validation_sheet_keys(
    features: &mut WorksheetFeatureTransport,
    mapper: impl Fn(u32) -> Option<u32> + Copy,
) {
    let validations = std::mem::take(&mut features.data_validations);
    features.data_validations = validations
        .into_iter()
        .filter_map(|(sheet, transport)| mapper(sheet).map(|sheet| (sheet, transport)))
        .collect();
    let dirty = std::mem::take(&mut features.data_validation_dirty);
    features.data_validation_dirty = dirty.into_iter().filter_map(mapper).collect();
}

fn mark_all_data_validation_sheets_dirty(st: &mut AppState) {
    let count = st.model.get_model().workbook.worksheets.len() as u32;
    st.worksheet_features.data_validation_dirty.extend(0..count);
}

fn quoted_sheet_name(name: &str) -> String {
    format!("'{}'!", name.replace('\'', "''"))
}

fn replace_bare_sheet_qualifier(formula: &str, old_name: &str, new_name: &str) -> String {
    if old_name.is_empty() {
        return formula.to_string();
    }
    let wanted = format!("{old_name}!");
    let replacement = quoted_sheet_name(new_name);
    let lower_formula = formula.to_lowercase();
    let lower_wanted = wanted.to_lowercase();
    let mut result = String::with_capacity(formula.len() + replacement.len());
    let mut cursor = 0usize;
    while let Some(relative) = lower_formula[cursor..].find(&lower_wanted) {
        let start = cursor + relative;
        let boundary = start == 0
            || formula[..start]
                .chars()
                .next_back()
                .map(|ch| !(ch.is_alphanumeric() || matches!(ch, '_' | '.' | '\'')))
                .unwrap_or(true);
        if boundary {
            result.push_str(&formula[cursor..start]);
            result.push_str(&replacement);
            cursor = start + wanted.len();
        } else {
            let ch = formula[start..].chars().next().unwrap();
            let end = start + ch.len_utf8();
            result.push_str(&formula[cursor..end]);
            cursor = end;
        }
    }
    result.push_str(&formula[cursor..]);
    result
}

fn rename_data_validation_sheet_references(st: &mut AppState, old_name: &str, new_name: &str) {
    let quoted_old = quoted_sheet_name(old_name);
    let quoted_new = quoted_sheet_name(new_name);
    let mut dirty = Vec::new();
    for (sheet, transport) in &mut st.worksheet_features.data_validations {
        let mut changed = false;
        for rule in &mut transport.rules {
            for formula in [&mut rule.formula1, &mut rule.formula2] {
                let Some(current) = formula.as_ref() else {
                    continue;
                };
                let quoted = current.replace(&quoted_old, &quoted_new);
                let renamed = replace_bare_sheet_qualifier(&quoted, old_name, new_name);
                if renamed != *current {
                    *formula = Some(renamed);
                    changed = true;
                }
            }
        }
        if changed {
            dirty.push(*sheet);
        }
    }
    st.worksheet_features.data_validation_dirty.extend(dirty);
}

fn remove_start_tag_attribute(fragment: &str, name: &str) -> String {
    let Some(tag_end) = fragment.find('>') else {
        return fragment.to_string();
    };
    let mut result = fragment.to_string();
    for quote in ['\"', '\''] {
        let needle = format!(" {name}={quote}");
        if let Some(start) = result[..tag_end].find(&needle) {
            let value_start = start + needle.len();
            if let Some(relative_end) = result[value_start..tag_end].find(quote) {
                result.replace_range(start..value_start + relative_end + 1, "");
                return result;
            }
        }
    }
    result
}

fn normalized_sqref(value: &str) -> String {
    let mut ranges: Vec<String> = value
        .split_whitespace()
        .map(|part| part.replace('$', "").to_ascii_uppercase())
        .collect();
    ranges.sort();
    ranges.join(" ")
}

fn normalized_cf_formula(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('=')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn cf_rule_semantic_key(rule: roxmltree::Node<'_, '_>) -> String {
    let mut fields = Vec::new();
    for name in [
        "type",
        "operator",
        "text",
        "timePeriod",
        "rank",
        "stdDev",
        "equalAverage",
        "percent",
        "bottom",
        "aboveAverage",
    ] {
        if let Some(value) = rule.attribute(name) {
            fields.push(format!("a:{name}={value}"));
        }
    }
    for formula in rule
        .descendants()
        .filter(|node| node.is_element() && matches!(node.tag_name().name(), "formula" | "f"))
    {
        fields.push(format!(
            "f:{}",
            normalized_cf_formula(formula.text().unwrap_or(""))
        ));
    }
    for node in rule.descendants().filter(|node| {
        node.is_element()
            && matches!(
                node.tag_name().name(),
                "cfvo" | "color" | "dataBar" | "iconSet"
            )
    }) {
        let mut attributes: Vec<String> = node
            .attributes()
            .filter(|attribute| {
                matches!(
                    attribute.name(),
                    "type"
                        | "val"
                        | "gte"
                        | "rgb"
                        | "theme"
                        | "indexed"
                        | "auto"
                        | "tint"
                        | "minLength"
                        | "maxLength"
                        | "showValue"
                        | "gradient"
                        | "iconSet"
                        | "reverse"
                        | "percent"
                        | "custom"
                )
            })
            .map(|attribute| format!("{}={}", attribute.name(), attribute.value()))
            .collect();
        attributes.sort();
        fields.push(format!(
            "{}[{}]",
            node.tag_name().name(),
            attributes.join(",")
        ));
    }
    fields.join("|")
}

fn supported_cf_rule_type(value: &str) -> bool {
    matches!(
        value,
        "cellIs"
            | "expression"
            | "containsText"
            | "notContainsText"
            | "beginsWith"
            | "endsWith"
            | "timePeriod"
            | "duplicateValues"
            | "uniqueValues"
            | "containsBlanks"
            | "notContainsBlanks"
            | "containsErrors"
            | "notContainsErrors"
            | "aboveAverage"
            | "top10"
            | "colorScale"
            | "dataBar"
            | "iconSet"
    )
}

#[derive(Clone)]
struct CfMergeRule {
    range: std::ops::Range<usize>,
    raw: String,
    key: String,
    sqref: String,
    rule_type: String,
    data_bar_x14_id: Option<String>,
}

#[derive(Clone, Default)]
struct CfExtensionMergePlan {
    data_bars: Vec<CfDataBarTransport>,
}

#[derive(Clone)]
struct CfDataBarTransport {
    generated_id: Option<String>,
    original_id: Option<String>,
}

#[derive(Clone)]
struct X14DataBarEntry {
    range: std::ops::Range<usize>,
    raw: String,
    id: String,
    sqref: String,
    sqref_range: Option<std::ops::Range<usize>>,
    rule_range: std::ops::Range<usize>,
}

fn cf_data_bar_x14_id(rule: roxmltree::Node<'_, '_>) -> Option<String> {
    if rule.attribute("type") != Some("dataBar") {
        return None;
    }
    rule.descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "id")
        .and_then(|node| node.text())
        .map(str::to_string)
}

fn collect_cf_merge_rules(xml: &str) -> Result<Vec<CfMergeRule>, String> {
    let document =
        roxmltree::Document::parse(xml).map_err(|e| format!("conditional formatting XML: {e}"))?;
    let mut rules = Vec::new();
    for container in document
        .root_element()
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "conditionalFormatting")
    {
        let sqref = normalized_sqref(container.attribute("sqref").unwrap_or(""));
        for rule in container
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "cfRule")
        {
            rules.push(CfMergeRule {
                range: rule.range(),
                raw: xml[rule.range()].to_string(),
                key: cf_rule_semantic_key(rule),
                sqref: sqref.clone(),
                rule_type: rule.attribute("type").unwrap_or("").to_string(),
                data_bar_x14_id: cf_data_bar_x14_id(rule),
            });
        }
    }
    Ok(rules)
}

fn collect_x14_data_bar_entries(xml: &str) -> Result<Vec<X14DataBarEntry>, String> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|e| format!("x14 conditional formatting XML: {e}"))?;
    let mut entries = Vec::new();
    for container in document.root_element().descendants().filter(|node| {
        node.is_element()
            && node.tag_name().name() == "conditionalFormatting"
            && node
                .parent()
                .is_some_and(|parent| parent.tag_name().name() == "conditionalFormattings")
    }) {
        let Some(rule) = container.children().find(|node| {
            node.is_element()
                && node.tag_name().name() == "cfRule"
                && node.attribute("type") == Some("dataBar")
        }) else {
            continue;
        };
        let Some(id) = rule.attribute("id") else {
            continue;
        };
        let container_range = container.range();
        let sqref = container
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "sqref");
        let sqref_range = sqref.map(|node| {
            let range = node.range();
            range.start - container_range.start..range.end - container_range.start
        });
        let rule_node_range = rule.range();
        entries.push(X14DataBarEntry {
            range: container_range.clone(),
            raw: xml[container_range.clone()].to_string(),
            id: id.to_string(),
            sqref: sqref.and_then(|node| node.text()).unwrap_or("").to_string(),
            sqref_range,
            rule_range: rule_node_range.start - container_range.start
                ..rule_node_range.end - container_range.start,
        });
    }
    Ok(entries)
}

fn patch_x14_entry_sqref(entry: &X14DataBarEntry, sqref: &str) -> String {
    let Some(range) = entry.sqref_range.clone() else {
        return entry.raw.clone();
    };
    let local = &entry.raw[range.clone()];
    let Some(content_start) = local.find('>').map(|offset| range.start + offset + 1) else {
        return entry.raw.clone();
    };
    let Some(content_end) = local.rfind("</").map(|offset| range.start + offset) else {
        return entry.raw.clone();
    };
    let mut raw = entry.raw.clone();
    raw.replace_range(content_start..content_end, &html_escape(sqref));
    raw
}

fn patch_x14_entry_rule_id(entry: &X14DataBarEntry, id: &str) -> String {
    let rule = &entry.raw[entry.rule_range.clone()];
    let patched = set_start_tag_attribute(rule, "id", id);
    let mut raw = entry.raw.clone();
    raw.replace_range(entry.rule_range.clone(), &patched);
    raw
}

fn apply_cf_extension_merge_plan(
    generated: &mut String,
    original: &str,
    plan: &CfExtensionMergePlan,
) -> Result<(), String> {
    if plan.data_bars.is_empty() {
        return Ok(());
    }
    let generated_entries = collect_x14_data_bar_entries(generated)?;
    let original_entries = collect_x14_data_bar_entries(original)?;
    let mut replacements: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for transport in &plan.data_bars {
        let Some(generated_id) = transport.generated_id.as_deref() else {
            continue;
        };
        let Some(generated_entry) = generated_entries
            .iter()
            .find(|entry| entry.id == generated_id)
        else {
            continue;
        };
        let replacement = match transport.original_id.as_deref() {
            Some(original_id) => {
                if let Some(original_entry) = original_entries
                    .iter()
                    .find(|entry| entry.id == original_id)
                {
                    patch_x14_entry_sqref(original_entry, &generated_entry.sqref)
                } else {
                    patch_x14_entry_rule_id(generated_entry, original_id)
                }
            }
            // The original rule had no x14 transport link.  Keeping IronCalc's generated
            // worksheet-level entry would leave an orphan after the raw main rule is restored.
            None => String::new(),
        };
        replacements.push((generated_entry.range.clone(), replacement));
    }
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (range, replacement) in replacements {
        generated.replace_range(range, &replacement);
    }
    Ok(())
}

fn patch_cf_transport_attributes(mut original: String, generated: &str) -> String {
    for name in ["priority", "dxfId", "stopIfTrue"] {
        if let Some(value) = xml_attr(generated, name) {
            original = set_start_tag_attribute(&original, name, &value);
        } else {
            original = remove_start_tag_attribute(&original, name);
        }
    }
    original
}

fn append_cf_rule(generated: &mut String, sqref: &str, rule: &str) -> Result<(), String> {
    let document = roxmltree::Document::parse(generated)
        .map_err(|e| format!("conditional formatting append XML: {e}"))?;
    let container = document.root_element().children().find(|node| {
        node.is_element()
            && node.tag_name().name() == "conditionalFormatting"
            && normalized_sqref(node.attribute("sqref").unwrap_or("")) == sqref
    });
    if let Some(container) = container {
        let range = container.range();
        let local = &generated[range.clone()];
        let relative = local.rfind("</").ok_or("malformed conditionalFormatting")?;
        let insert_at = range.start + relative;
        drop(document);
        generated.insert_str(insert_at, rule);
    } else {
        drop(document);
        let escaped = html_escape(sqref);
        insert_worksheet_fragment(
            generated,
            "conditionalFormatting",
            &format!("<conditionalFormatting sqref=\"{escaped}\">{rule}</conditionalFormatting>"),
        )?;
    }
    Ok(())
}

fn merge_conditional_formatting_rules(
    generated: &mut String,
    original: &str,
) -> Result<CfExtensionMergePlan, String> {
    merge_root_namespace_declarations(generated, original);
    let generated_rules = collect_cf_merge_rules(generated)?;
    let original_rules = collect_cf_merge_rules(original)?;
    let mut used_original = vec![false; original_rules.len()];
    let mut replacements = Vec::new();
    let mut extension_plan = CfExtensionMergePlan::default();
    for generated_rule in &generated_rules {
        let match_index = original_rules
            .iter()
            .enumerate()
            .find(|(index, original_rule)| {
                !used_original[*index]
                    && original_rule.key == generated_rule.key
                    && original_rule.sqref == generated_rule.sqref
            })
            .or_else(|| {
                original_rules
                    .iter()
                    .enumerate()
                    .find(|(index, original_rule)| {
                        !used_original[*index] && original_rule.key == generated_rule.key
                    })
            })
            .map(|(index, _)| index);
        if let Some(index) = match_index {
            used_original[index] = true;
            if generated_rule.rule_type == "dataBar" && original_rules[index].rule_type == "dataBar"
            {
                extension_plan.data_bars.push(CfDataBarTransport {
                    generated_id: generated_rule.data_bar_x14_id.clone(),
                    original_id: original_rules[index].data_bar_x14_id.clone(),
                });
            }
            replacements.push((
                generated_rule.range.clone(),
                patch_cf_transport_attributes(
                    original_rules[index].raw.clone(),
                    &generated_rule.raw,
                ),
            ));
        }
    }
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (range, replacement) in replacements {
        generated.replace_range(range, &replacement);
    }
    for (index, original_rule) in original_rules.iter().enumerate() {
        if !used_original[index] && !supported_cf_rule_type(&original_rule.rule_type) {
            append_cf_rule(generated, &original_rule.sqref, &original_rule.raw)?;
        }
    }
    Ok(extension_plan)
}

fn merge_extension_list_children(
    generated: &mut String,
    original: &str,
    plan: &CfExtensionMergePlan,
) -> Result<(), String> {
    merge_root_namespace_declarations(generated, original);
    apply_cf_extension_merge_plan(generated, original, plan)?;
    let original_document =
        roxmltree::Document::parse(original).map_err(|e| format!("original extLst XML: {e}"))?;
    let Some(original_list) = original_document
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "extLst")
    else {
        return Ok(());
    };
    let generated_document =
        roxmltree::Document::parse(generated).map_err(|e| format!("generated extLst XML: {e}"))?;
    let generated_list = generated_document
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "extLst");
    let existing_uris: std::collections::HashSet<String> = generated_list
        .into_iter()
        .flat_map(|list| list.children())
        .filter(|node| node.is_element() && node.tag_name().name() == "ext")
        .filter_map(|node| node.attribute("uri").map(str::to_string))
        .collect();
    drop(generated_document);
    let additions: Vec<String> = original_list
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "ext")
        .filter(|node| {
            let has_cf = node.descendants().any(|child| {
                child.is_element() && child.tag_name().name() == "conditionalFormattings"
            });
            let has_dv = node
                .descendants()
                .any(|child| child.is_element() && child.tag_name().name() == "dataValidations");
            (!has_cf || has_dv)
                && node
                    .attribute("uri")
                    .map(|uri| !existing_uris.contains(uri))
                    .unwrap_or(true)
        })
        .map(|node| original[node.range()].to_string())
        .collect();
    if additions.is_empty() {
        return Ok(());
    }
    let generated_document = roxmltree::Document::parse(generated)
        .map_err(|e| format!("generated extLst append XML: {e}"))?;
    if let Some(list) = generated_document
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "extLst")
    {
        let range = list.range();
        let local = &generated[range.clone()];
        let insert_at = range.start + local.rfind("</").ok_or("malformed extLst")?;
        drop(generated_document);
        generated.insert_str(insert_at, &additions.join(""));
    } else {
        drop(generated_document);
        insert_worksheet_fragment(
            generated,
            "extLst",
            &format!("<extLst>{}</extLst>", additions.join("")),
        )?;
    }
    Ok(())
}

fn conditional_formatting_unchanged(st: &AppState, sheet: u32) -> bool {
    let Some(baseline) = st
        .worksheet_features
        .baseline_conditional_formatting
        .get(&sheet)
    else {
        return false;
    };
    st.model
        .get_conditional_formatting_list(sheet)
        .ok()
        .and_then(|list| serde_json::to_string(&list).ok())
        .map(|current| current == *baseline)
        .unwrap_or(false)
}

fn restore_worksheet_feature_subtrees(
    st: &AppState,
    sheet: u32,
    generated: &mut String,
    original: &str,
) -> Result<(), String> {
    // Untouched data validation remains byte-exact.  Edited standard rules are injected from the
    // typed transport after all opaque OPC parts have been restored; x14 validators in extLst stay
    // opaque and survive either branch.
    if !st.worksheet_features.data_validation_dirty.contains(&sheet) {
        replace_top_level_from_original(generated, original, "dataValidations")?;
    }
    if conditional_formatting_unchanged(st, sheet) {
        replace_top_level_from_original(generated, original, "conditionalFormatting")?;
        replace_top_level_from_original(generated, original, "extLst")?;
    } else {
        let extension_plan = merge_conditional_formatting_rules(generated, original)?;
        merge_extension_list_children(generated, original, &extension_plan)?;
    }
    Ok(())
}

fn merge_original_dxfs(generated: &[u8], original: &[u8]) -> Result<Vec<u8>, String> {
    let mut generated_xml = std::str::from_utf8(generated)
        .map_err(|e| format!("generated styles utf8: {e}"))?
        .to_string();
    let original_xml =
        std::str::from_utf8(original).map_err(|e| format!("original styles utf8: {e}"))?;
    merge_root_namespace_declarations(&mut generated_xml, original_xml);
    let original_document = roxmltree::Document::parse(original_xml)
        .map_err(|e| format!("original styles XML: {e}"))?;
    let Some(original_dxfs) = original_document
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "dxfs")
    else {
        return Ok(generated_xml.into_bytes());
    };
    let original_nodes: Vec<String> = original_dxfs
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "dxf")
        .map(|node| original_xml[node.range()].to_string())
        .collect();
    if original_nodes.is_empty() {
        return Ok(generated_xml.into_bytes());
    }
    let generated_document = roxmltree::Document::parse(&generated_xml)
        .map_err(|e| format!("generated styles XML: {e}"))?;
    let generated_dxfs = generated_document
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "dxfs");
    let Some(generated_dxfs) = generated_dxfs else {
        drop(generated_document);
        let document = roxmltree::Document::parse(&generated_xml)
            .map_err(|e| format!("generated styles root XML: {e}"))?;
        let root = document.root_element();
        let insert_at = generated_xml
            .rfind(&format!("</{}>", root.tag_name().name()))
            .ok_or("malformed styles root")?;
        drop(document);
        generated_xml.insert_str(
            insert_at,
            &format!(
                "<dxfs count=\"{}\">{}</dxfs>",
                original_nodes.len(),
                original_nodes.join("")
            ),
        );
        return Ok(generated_xml.into_bytes());
    };
    let container_range = generated_dxfs.range();
    let generated_nodes: Vec<std::ops::Range<usize>> = generated_dxfs
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "dxf")
        .map(|node| node.range())
        .collect();
    drop(generated_document);
    let mut replacements: Vec<(std::ops::Range<usize>, String)> = generated_nodes
        .iter()
        .zip(original_nodes.iter())
        .map(|(range, raw)| (range.clone(), raw.clone()))
        .collect();
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (range, raw) in replacements {
        generated_xml.replace_range(range, &raw);
    }
    if original_nodes.len() > generated_nodes.len() {
        let document = roxmltree::Document::parse(&generated_xml)
            .map_err(|e| format!("generated dxfs append XML: {e}"))?;
        let dxfs = document
            .root_element()
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "dxfs")
            .ok_or("missing generated dxfs")?;
        let range = dxfs.range();
        let local = &generated_xml[range.clone()];
        let insert_at = range.start + local.rfind("</").ok_or("malformed dxfs")?;
        drop(document);
        generated_xml.insert_str(insert_at, &original_nodes[generated_nodes.len()..].join(""));
    }
    let document = roxmltree::Document::parse(&generated_xml)
        .map_err(|e| format!("generated dxfs count XML: {e}"))?;
    let dxfs = document
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "dxfs")
        .ok_or("missing dxfs after merge")?;
    let range = dxfs.range();
    let count = dxfs
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "dxf")
        .count();
    let raw = set_start_tag_attribute(&generated_xml[range.clone()], "count", &count.to_string());
    drop(document);
    generated_xml.replace_range(range, &raw);
    let _ = container_range;
    Ok(generated_xml.into_bytes())
}

fn remove_theme_content_type_overrides(xml: &str) -> Result<String, String> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|e| format!("content types XML while replacing theme: {e}"))?;
    let mut ranges: Vec<std::ops::Range<usize>> = document
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name("Override"))
        .filter(|node| {
            node.attribute("PartName")
                .map(|value| value.trim_start_matches('/').starts_with("xl/theme/"))
                .unwrap_or(false)
        })
        .map(|node| node.range())
        .collect();
    drop(document);
    ranges.sort_by_key(|range| range.start);
    let mut result = xml.to_string();
    for range in ranges.into_iter().rev() {
        result.replace_range(range, "");
    }
    Ok(result)
}

fn merge_content_types(
    generated: &[u8],
    original: &[u8],
    snapshot: &OpcPackageSnapshot,
    additional_parts: &std::collections::HashSet<String>,
    cloned_from: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<u8>, String> {
    let mut generated_xml = std::str::from_utf8(generated)
        .map_err(|e| format!("content types utf8: {e}"))?
        .to_string();
    let imported_theme_part = snapshot_workbook_theme_part(snapshot);
    if imported_theme_part.is_some() {
        generated_xml = remove_theme_content_type_overrides(&generated_xml)?;
    }
    let original_xml =
        std::str::from_utf8(original).map_err(|e| format!("original content types utf8: {e}"))?;
    let original_doc = roxmltree::Document::parse(original_xml)
        .map_err(|e| format!("original content types XML: {e}"))?;

    if snapshot.macro_enabled {
        let original_workbook = original_doc
            .descendants()
            .find(|n| {
                n.is_element()
                    && n.has_tag_name("Override")
                    && n.attribute("PartName") == Some("/xl/workbook.xml")
            })
            .map(|n| original_xml[n.range()].to_string());
        if let Some(workbook_override) = original_workbook {
            let generated_doc = roxmltree::Document::parse(&generated_xml)
                .map_err(|e| format!("generated content types XML: {e}"))?;
            if let Some(node) = generated_doc.descendants().find(|n| {
                n.is_element()
                    && n.has_tag_name("Override")
                    && n.attribute("PartName") == Some("/xl/workbook.xml")
            }) {
                let range = node.range();
                drop(generated_doc);
                generated_xml.replace_range(range, &workbook_override);
            }
        }
    }

    let generated_doc = roxmltree::Document::parse(&generated_xml)
        .map_err(|e| format!("generated content types XML: {e}"))?;
    let mut default_extensions: std::collections::HashSet<String> = generated_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("Default"))
        .filter_map(|n| n.attribute("Extension").map(|v| v.to_ascii_lowercase()))
        .collect();
    let mut override_parts: std::collections::HashSet<String> = generated_doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("Override"))
        .filter_map(|n| {
            n.attribute("PartName")
                .map(|v| v.trim_start_matches('/').to_string())
        })
        .collect();
    drop(generated_doc);

    let mut additions = String::new();
    for node in original_doc
        .root_element()
        .children()
        .filter(|n| n.is_element())
    {
        if node.has_tag_name("Default") {
            let Some(extension) = node.attribute("Extension") else {
                continue;
            };
            let ext_lower = extension.to_ascii_lowercase();
            let used = snapshot.parts.keys().any(|part| {
                (opc_part_is_preservable(part) || additional_parts.contains(part))
                    && part
                        .rsplit_once('.')
                        .map(|(_, ext)| ext.eq_ignore_ascii_case(extension))
                        .unwrap_or(false)
            });
            if used && default_extensions.insert(ext_lower) {
                additions.push_str(&original_xml[node.range()]);
            }
        } else if node.has_tag_name("Override") {
            let Some(part_name) = node.attribute("PartName") else {
                continue;
            };
            let normalized = part_name.trim_start_matches('/');
            if normalized.starts_with("xl/theme/")
                && imported_theme_part.as_deref() != Some(normalized)
            {
                continue;
            }
            if snapshot.parts.contains_key(normalized)
                && (opc_part_is_preservable(normalized) || additional_parts.contains(normalized))
                && override_parts.insert(normalized.to_string())
            {
                additions.push_str(&original_xml[node.range()]);
            }
        }
    }
    if let Some(theme_part) = imported_theme_part.as_deref() {
        if override_parts.insert(theme_part.to_string()) {
            additions.push_str(&format!(
                "<Override PartName=\"/{}\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>",
                html_escape(theme_part),
            ));
        }
    }
    for (part, source) in cloned_from {
        if override_parts.contains(part) {
            continue;
        }
        if let Some(source_override) = original_doc.descendants().find(|node| {
            node.is_element()
                && node.has_tag_name("Override")
                && node
                    .attribute("PartName")
                    .map(|value| value.trim_start_matches('/'))
                    == Some(source.as_str())
        }) {
            let replacement = set_start_tag_attribute(
                &original_xml[source_override.range()],
                "PartName",
                &format!("/{part}"),
            );
            additions.push_str(&replacement);
            override_parts.insert(part.clone());
        }
    }
    for part in additional_parts
        .iter()
        .filter(|part| !snapshot.parts.contains_key(*part))
    {
        if override_parts.contains(part) {
            continue;
        }
        let content_type = if part.starts_with("xl/charts/") && part.ends_with(".xml") {
            Some("application/vnd.openxmlformats-officedocument.drawingml.chart+xml")
        } else if part.starts_with("xl/diagrams/unicellData") && part.ends_with(".xml") {
            Some("application/vnd.openxmlformats-officedocument.drawingml.diagramData+xml")
        } else if part.starts_with("xl/diagrams/unicellLayout") && part.ends_with(".xml") {
            Some("application/vnd.openxmlformats-officedocument.drawingml.diagramLayout+xml")
        } else if part.starts_with("xl/diagrams/unicellQuickStyle") && part.ends_with(".xml") {
            Some("application/vnd.openxmlformats-officedocument.drawingml.diagramStyle+xml")
        } else if part.starts_with("xl/diagrams/unicellColors") && part.ends_with(".xml") {
            Some("application/vnd.openxmlformats-officedocument.drawingml.diagramColors+xml")
        } else if part.starts_with("xl/drawings/unicellNativeDrawing") && part.ends_with(".xml") {
            Some("application/vnd.openxmlformats-officedocument.drawing+xml")
        } else {
            None
        };
        if let Some(content_type) = content_type {
            if override_parts.insert(part.clone()) {
                additions.push_str(&format!(
                    "<Override PartName=\"/{}\" ContentType=\"{}\"/>",
                    html_escape(part),
                    content_type,
                ));
            }
        }
    }
    if !additions.is_empty() {
        let close = generated_xml
            .rfind("</Types>")
            .ok_or("malformed [Content_Types].xml")?;
        generated_xml.insert_str(close, &additions);
    }
    Ok(generated_xml.into_bytes())
}

fn set_start_tag_attribute(fragment: &str, name: &str, value: &str) -> String {
    let Some(tag_end) = fragment.find('>') else {
        return fragment.to_string();
    };
    let mut result = fragment.to_string();
    for quote in ['\"', '\''] {
        let needle = format!(" {name}={quote}");
        if let Some(start) = result[..tag_end].find(&needle) {
            let value_start = start + needle.len();
            if let Some(relative_end) = result[value_start..tag_end].find(quote) {
                result.replace_range(value_start..value_start + relative_end, value);
                return result;
            }
        }
    }
    let insert_at = if tag_end > 0 && result.as_bytes()[tag_end - 1] == b'/' {
        tag_end - 1
    } else {
        tag_end
    };
    result.insert_str(insert_at, &format!(" {name}=\"{value}\""));
    result
}

fn replace_cell_formula_fragment(cell_xml: &str, raw_formula: &str) -> String {
    let Some(open_end) = cell_xml.find('>') else {
        return cell_xml.to_string();
    };
    if open_end > 0 && cell_xml.as_bytes()[open_end - 1] == b'/' {
        let mut result = cell_xml.to_string();
        result.replace_range(open_end - 1..=open_end, ">");
        result.push_str(raw_formula);
        result.push_str("</c>");
        return result;
    }
    if let Some(formula_start) = cell_xml.find("<f") {
        let after_start = &cell_xml[formula_start..];
        let formula_end = if let Some(self_close) = after_start.find("/>") {
            let normal_close = after_start.find("</f>");
            match normal_close {
                Some(close) if close < self_close => formula_start + close + 4,
                _ => formula_start + self_close + 2,
            }
        } else if let Some(close) = after_start.find("</f>") {
            formula_start + close + 4
        } else {
            formula_start
        };
        if formula_end > formula_start {
            let mut result = cell_xml.to_string();
            result.replace_range(formula_start..formula_end, raw_formula);
            return result;
        }
    }
    let mut result = cell_xml.to_string();
    result.insert_str(open_end + 1, raw_formula);
    result
}

fn restore_formula_transport_for_sheet(
    st: &AppState,
    sheet_path: &str,
    generated: &mut String,
) -> Result<(), String> {
    let Some(transport) = st.formula_transport.sheets.get(sheet_path) else {
        return Ok(());
    };
    for group in &transport.groups {
        let unchanged = group.cells.iter().all(|cell| {
            let Some((row, column)) = parse_a1(&cell.reference) else {
                return false;
            };
            st.model
                .get_cell_content(transport.sheet_index, row, column)
                .map(|current| current == cell.baseline_content)
                .unwrap_or(false)
        });
        if !unchanged {
            continue;
        }
        for preserved in &group.cells {
            let document = roxmltree::Document::parse(generated)
                .map_err(|e| format!("formula transport sheet XML: {e}"))?;
            let Some(cell) = document.descendants().find(|n| {
                n.is_element()
                    && n.has_tag_name("c")
                    && n.attribute("r") == Some(preserved.reference.as_str())
            }) else {
                continue;
            };
            let range = cell.range();
            let mut cell_xml =
                replace_cell_formula_fragment(&generated[range.clone()], &preserved.raw_formula);
            if let Some(metadata) = &preserved.cell_metadata {
                cell_xml = set_start_tag_attribute(&cell_xml, "cm", metadata);
            }
            if let Some(metadata) = &preserved.value_metadata {
                cell_xml = set_start_tag_attribute(&cell_xml, "vm", metadata);
            }
            drop(document);
            generated.replace_range(range, &cell_xml);
        }
    }
    Ok(())
}

fn json_number(value: &Value, key: &str, fallback: f64) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(fallback)
}

fn native_object_absolute_geometry(
    st: &AppState,
    sheet: u32,
    object: &Value,
) -> (f64, f64, f64, f64) {
    let mut x = json_number(object, "x", 0.0);
    let mut y = json_number(object, "y", 0.0);
    if object["mode"].as_str().unwrap_or("abs") != "abs" {
        let column = json_number(object, "c", 1.0).round().max(1.0) as i32;
        let row = json_number(object, "r", 1.0).round().max(1.0) as i32;
        for current in 1..column {
            x += st.model.get_column_width(sheet, current).unwrap_or(100.0);
        }
        for current in 1..row {
            y += st.model.get_row_height(sheet, current).unwrap_or(21.0);
        }
    }
    (
        x,
        y,
        json_number(object, "w", 100.0).max(1.0),
        json_number(object, "h", 100.0).max(1.0),
    )
}

fn drawing_column_marker(model: &UserModel, sheet: u32, pixels: f64) -> (i64, i64) {
    let mut remaining = pixels.max(0.0);
    for index in 0..MAX_COLS {
        let width = model
            .get_column_width(sheet, index + 1)
            .unwrap_or(100.0)
            .max(0.01);
        if remaining < width || index == MAX_COLS - 1 {
            return (index as i64, (remaining * EMU_PER_PX as f64).round() as i64);
        }
        remaining -= width;
    }
    (0, 0)
}

fn drawing_row_marker(model: &UserModel, sheet: u32, pixels: f64) -> (i64, i64) {
    let mut remaining = pixels.max(0.0);
    for index in 0..MAX_ROWS {
        let height = model
            .get_row_height(sheet, index + 1)
            .unwrap_or(21.0)
            .max(0.01);
        if remaining < height || index == MAX_ROWS - 1 {
            return (index as i64, (remaining * EMU_PER_PX as f64).round() as i64);
        }
        remaining -= height;
    }
    (0, 0)
}

fn drawing_marker_xml(local: &str, model: &UserModel, sheet: u32, x: f64, y: f64) -> String {
    let (column, column_offset) = drawing_column_marker(model, sheet, x);
    let (row, row_offset) = drawing_row_marker(model, sheet, y);
    format!(
        "<xdr:{local}><xdr:col>{column}</xdr:col><xdr:colOff>{column_offset}</xdr:colOff><xdr:row>{row}</xdr:row><xdr:rowOff>{row_offset}</xdr:rowOff></xdr:{local}>"
    )
}

fn replace_drawing_child(fragment: &str, local: &str, replacement: &str) -> String {
    for prefix in ["xdr:", ""] {
        let open = format!("<{prefix}{local}");
        let Some(start) = fragment.find(&open) else {
            continue;
        };
        let close = format!("</{prefix}{local}>");
        if let Some(relative) = fragment[start..].find(&close) {
            let end = start + relative + close.len();
            let mut result = fragment.to_string();
            result.replace_range(start..end, replacement);
            return result;
        }
    }
    fragment.to_string()
}

fn set_drawing_element_attributes(
    fragment: &str,
    prefix: &str,
    local: &str,
    attributes: &[(&str, String)],
) -> String {
    let needle = if prefix.is_empty() {
        format!("<{local}")
    } else {
        format!("<{prefix}:{local}")
    };
    let Some(start) = fragment.find(&needle) else {
        return fragment.to_string();
    };
    let Some(relative_end) = fragment[start..].find('>') else {
        return fragment.to_string();
    };
    let end = start + relative_end + 1;
    let mut tag = fragment[start..end].to_string();
    for (name, value) in attributes {
        tag = set_start_tag_attribute(&tag, name, value);
    }
    let mut result = fragment.to_string();
    result.replace_range(start..end, &tag);
    result
}

fn rewrite_native_anchor_geometry(
    raw: &str,
    anchor_kind: &str,
    model: &UserModel,
    sheet: u32,
    geometry: (f64, f64, f64, f64),
    resize: bool,
) -> String {
    let (x, y, width, height) = geometry;
    let cx = (width * EMU_PER_PX as f64).round() as i64;
    let cy = (height * EMU_PER_PX as f64).round() as i64;
    let mut result = raw.to_string();
    match anchor_kind {
        "absoluteAnchor" => {
            result = set_drawing_element_attributes(
                &result,
                "xdr",
                "pos",
                &[
                    ("x", ((x * EMU_PER_PX as f64).round() as i64).to_string()),
                    ("y", ((y * EMU_PER_PX as f64).round() as i64).to_string()),
                ],
            );
            if !result.contains("<xdr:pos") {
                result = set_drawing_element_attributes(
                    &result,
                    "",
                    "pos",
                    &[
                        ("x", ((x * EMU_PER_PX as f64).round() as i64).to_string()),
                        ("y", ((y * EMU_PER_PX as f64).round() as i64).to_string()),
                    ],
                );
            }
            if resize {
                result = set_drawing_element_attributes(
                    &result,
                    "xdr",
                    "ext",
                    &[("cx", cx.to_string()), ("cy", cy.to_string())],
                );
                if !result.contains("<xdr:ext") {
                    result = set_drawing_element_attributes(
                        &result,
                        "",
                        "ext",
                        &[("cx", cx.to_string()), ("cy", cy.to_string())],
                    );
                }
            }
        }
        "twoCellAnchor" => {
            result = replace_drawing_child(
                &result,
                "from",
                &drawing_marker_xml("from", model, sheet, x, y),
            );
            result = replace_drawing_child(
                &result,
                "to",
                &drawing_marker_xml("to", model, sheet, x + width, y + height),
            );
        }
        _ => {
            result = replace_drawing_child(
                &result,
                "from",
                &drawing_marker_xml("from", model, sheet, x, y),
            );
            if resize {
                result = set_drawing_element_attributes(
                    &result,
                    "xdr",
                    "ext",
                    &[("cx", cx.to_string()), ("cy", cy.to_string())],
                );
                if !result.contains("<xdr:ext") {
                    result = set_drawing_element_attributes(
                        &result,
                        "",
                        "ext",
                        &[("cx", cx.to_string()), ("cy", cy.to_string())],
                    );
                }
            }
        }
    }
    if resize {
        // Update only the object's outer transform extent.  Child coordinate systems (`chExt`),
        // rotations, flips, gradients and effects remain byte-for-byte intact.
        result = set_drawing_element_attributes(
            &result,
            "a",
            "ext",
            &[("cx", cx.to_string()), ("cy", cy.to_string())],
        );
    }
    result
}

fn drawing_relationships_path(owner: &str) -> String {
    match owner.rsplit_once('/') {
        Some((directory, file)) => format!("{directory}/_rels/{file}.rels"),
        None => format!("_rels/{owner}.rels"),
    }
}

fn remap_drawing_relationship_attributes(
    fragment: String,
    id_map: &std::collections::HashMap<String, String>,
) -> String {
    remap_exact_xml_attribute_values(
        fragment,
        id_map,
        &["r:id", "r:embed", "r:link", "r:dm", "r:lo", "r:qs", "r:cs"],
    )
}

fn remap_diagram_data_relationship(
    mut xml: String,
    id_map: &std::collections::HashMap<String, String>,
) -> String {
    const DSP_NS: &str = "http://schemas.microsoft.com/office/drawing/2008/diagram";
    let Ok(document) = roxmltree::Document::parse(&xml) else {
        return xml;
    };
    let mut replacements: Vec<(std::ops::Range<usize>, String)> = document
        .descendants()
        .filter(|node| {
            node.is_element()
                && node.tag_name().name() == "dataModelExt"
                && node.tag_name().namespace() == Some(DSP_NS)
        })
        .filter_map(|node| {
            let old = node.attribute("relId")?;
            let new = id_map.get(old)?;
            if old == new {
                return None;
            }
            let range = node.range();
            Some((
                range.clone(),
                set_start_tag_attribute(&xml[range], "relId", new),
            ))
        })
        .collect();
    drop(document);
    replacements.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (range, replacement) in replacements {
        xml.replace_range(range, &replacement);
    }
    xml
}

fn remap_exact_xml_attribute_values(
    mut xml: String,
    id_map: &std::collections::HashMap<String, String>,
    attribute_names: &[&str],
) -> String {
    let mut replacements = Vec::new();
    for (index, (old, new)) in id_map.iter().filter(|(old, new)| old != new).enumerate() {
        let mut salt = 0usize;
        let placeholder = loop {
            let candidate = format!("__UNICELL_REL_REMAP_{index}_{salt}__");
            if !xml.contains(&candidate) {
                break candidate;
            }
            salt += 1;
        };
        for name in attribute_names {
            for quote in ['\"', '\''] {
                xml = xml.replace(
                    &format!("{name}={quote}{old}{quote}"),
                    &format!("{name}={quote}{placeholder}{quote}"),
                );
            }
        }
        replacements.push((placeholder, new));
    }
    for (placeholder, new) in replacements {
        xml = xml.replace(&placeholder, new);
    }
    xml
}

fn strip_smartart_diagram_cache_extension(xml: &str) -> Result<String, String> {
    const DSP_NS: &str = "http://schemas.microsoft.com/office/drawing/2008/diagram";
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| format!("SmartArt diagram data XML: {error}"))?;
    let mut ranges: Vec<std::ops::Range<usize>> = document
        .descendants()
        .filter(|node| {
            node.is_element()
                && node.tag_name().name() == "dataModelExt"
                && node.tag_name().namespace() == Some(DSP_NS)
        })
        .map(|node| {
            node.parent()
                .filter(|parent| {
                    parent.is_element()
                        && parent.tag_name().name() == "ext"
                        && parent.children().filter(|child| child.is_element()).count() == 1
                })
                .unwrap_or(node)
                .range()
        })
        .collect();
    ranges.sort_by(|left, right| right.start.cmp(&left.start));
    ranges.dedup_by(|left, right| left.start == right.start && left.end == right.end);
    let mut result = xml.to_string();
    for range in ranges {
        result.replace_range(range, "");
    }
    Ok(result)
}

fn copy_native_dependency_closure(
    snapshot: &OpcPackageSnapshot,
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    part: &str,
    copied: &mut std::collections::HashSet<String>,
    visited: &mut std::collections::HashSet<String>,
) -> Result<(), String> {
    let Some(part) = normalize_opc_part_name(part) else {
        return Ok(());
    };
    if !visited.insert(part.clone()) || part.starts_with("_xmlsignatures/") {
        return Ok(());
    }
    let Some(bytes) = snapshot.parts.get(&part) else {
        return Ok(());
    };
    parts.insert(part.clone(), bytes.clone());
    copied.insert(part.clone());
    let rels_path = drawing_relationships_path(&part);
    let Some(rels_bytes) = snapshot.parts.get(&rels_path) else {
        return Ok(());
    };
    let rels_xml =
        std::str::from_utf8(rels_bytes).map_err(|e| format!("native dependency rels utf8: {e}"))?;
    let document = roxmltree::Document::parse(rels_xml)
        .map_err(|e| format!("native dependency rels XML: {e}"))?;
    parts.insert(rels_path.clone(), rels_bytes.clone());
    copied.insert(rels_path);
    let targets: Vec<String> = document
        .descendants()
        .filter(|node| {
            node.is_element()
                && node.has_tag_name("Relationship")
                && node.attribute("TargetMode") != Some("External")
        })
        .filter_map(|node| node.attribute("Target"))
        .map(|target| target.split('#').next().unwrap_or(target))
        .map(|target| resolve_rel_path(&path_dir(&part), target))
        .collect();
    for target in targets {
        copy_native_dependency_closure(snapshot, parts, &target, copied, visited)?;
    }
    Ok(())
}

fn drawing_anchor_relationship_ids(
    anchor: &str,
    relationships_xml: &str,
) -> Result<std::collections::HashSet<String>, String> {
    let document = roxmltree::Document::parse(relationships_xml)
        .map_err(|e| format!("native drawing rels XML: {e}"))?;
    Ok(document
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
        .filter_map(|node| node.attribute("Id"))
        .filter(|id| anchor.contains(&format!("=\"{id}\"")) || anchor.contains(&format!("='{id}'")))
        .map(str::to_string)
        .collect())
}

fn merge_selected_drawing_relationships(
    generated: Option<&[u8]>,
    original: &[u8],
    original_owner: &str,
    referenced_ids: &std::collections::HashSet<String>,
    snapshot: &OpcPackageSnapshot,
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    copied: &mut std::collections::HashSet<String>,
) -> Result<(Vec<u8>, std::collections::HashMap<String, String>), String> {
    let original_xml =
        std::str::from_utf8(original).map_err(|e| format!("native drawing rels utf8: {e}"))?;
    let original_document = roxmltree::Document::parse(original_xml)
        .map_err(|e| format!("native drawing rels XML: {e}"))?;
    let mut generated_xml = generated.map(|bytes| String::from_utf8_lossy(bytes).to_string())
        .unwrap_or_else(|| "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_string());
    let generated_document = roxmltree::Document::parse(&generated_xml)
        .map_err(|e| format!("generated drawing rels XML: {e}"))?;
    let mut used_ids: std::collections::HashSet<String> = generated_document
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
        .filter_map(|node| node.attribute("Id").map(str::to_string))
        .collect();
    let mut existing: std::collections::HashMap<(String, String, String), String> =
        generated_document
            .descendants()
            .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
            .map(|node| {
                (
                    (
                        node.attribute("Type").unwrap_or("").to_string(),
                        node.attribute("Target").unwrap_or("").to_string(),
                        node.attribute("TargetMode").unwrap_or("").to_string(),
                    ),
                    node.attribute("Id").unwrap_or("").to_string(),
                )
            })
            .collect();
    drop(generated_document);
    let mut id_map = std::collections::HashMap::new();
    let mut appended = String::new();
    let mut suffix = 1usize;
    for relationship in original_document.descendants().filter(|node| {
        node.is_element()
            && node.has_tag_name("Relationship")
            && node
                .attribute("Id")
                .map(|id| referenced_ids.contains(id))
                .unwrap_or(false)
    }) {
        let old_id = relationship.attribute("Id").unwrap_or("").to_string();
        let key = (
            relationship.attribute("Type").unwrap_or("").to_string(),
            relationship.attribute("Target").unwrap_or("").to_string(),
            relationship
                .attribute("TargetMode")
                .unwrap_or("")
                .to_string(),
        );
        if let Some(id) = existing.get(&key) {
            id_map.insert(old_id, id.clone());
        } else {
            let mut new_id = old_id.clone();
            if new_id.is_empty() || used_ids.contains(&new_id) {
                loop {
                    let candidate = format!("rId{suffix}");
                    suffix += 1;
                    if !used_ids.contains(&candidate) {
                        new_id = candidate;
                        break;
                    }
                }
            }
            appended.push_str(&replace_relationship_id(
                &original_xml[relationship.range()],
                &old_id,
                &new_id,
            ));
            used_ids.insert(new_id.clone());
            existing.insert(key, new_id.clone());
            id_map.insert(old_id, new_id);
        }
        if relationship.attribute("TargetMode") != Some("External") {
            if let Some(target) = relationship.attribute("Target") {
                let target = target.split('#').next().unwrap_or(target);
                let resolved = resolve_rel_path(&path_dir(original_owner), target);
                copy_native_dependency_closure(
                    snapshot,
                    parts,
                    &resolved,
                    copied,
                    &mut std::collections::HashSet::new(),
                )?;
            }
        }
    }
    if !appended.is_empty() {
        let close = generated_xml
            .rfind("</Relationships>")
            .ok_or("malformed generated drawing relationships")?;
        generated_xml.insert_str(close, &appended);
    }
    Ok((generated_xml.into_bytes(), id_map))
}

#[derive(Clone, Debug)]
struct NativeContentRelationship {
    id: String,
    role: String,
    relationship_type: String,
    target_mode: String,
    resolved_part: String,
}

#[derive(Default)]
struct NativeCompositionResult {
    parts: std::collections::HashSet<String>,
    cloned_from: std::collections::BTreeMap<String, String>,
}

fn native_content_relationships(
    anchor: &str,
    relationships_xml: &str,
    owner: &str,
    kind: &str,
    snapshot: &OpcPackageSnapshot,
) -> Result<Vec<NativeContentRelationship>, String> {
    let requested: Vec<(&str, Option<String>)> = match kind {
        "chart" => vec![("chart", xml_local_element_attr(anchor, "chart", "r:id"))],
        "smartart" => vec![
            (
                "diagramData",
                drawing_any_element_attr(anchor, "relIds", "r:dm"),
            ),
            (
                "diagramLayout",
                drawing_any_element_attr(anchor, "relIds", "r:lo"),
            ),
            (
                "diagramQuickStyle",
                drawing_any_element_attr(anchor, "relIds", "r:qs"),
            ),
            (
                "diagramColors",
                drawing_any_element_attr(anchor, "relIds", "r:cs"),
            ),
        ],
        _ => Vec::new(),
    };
    if requested.is_empty() || relationships_xml.is_empty() {
        return Ok(Vec::new());
    }
    let document = roxmltree::Document::parse(relationships_xml)
        .map_err(|e| format!("native content relationships XML: {e}"))?;
    let mut result = Vec::new();
    for (role, relationship_id) in requested {
        let Some(relationship_id) = relationship_id else {
            continue;
        };
        let Some(relationship) = document.descendants().find(|node| {
            node.is_element()
                && node.has_tag_name("Relationship")
                && node.attribute("Id") == Some(relationship_id.as_str())
        }) else {
            continue;
        };
        let target = relationship.attribute("Target").unwrap_or("").to_string();
        result.push(NativeContentRelationship {
            id: relationship_id,
            role: role.to_string(),
            relationship_type: relationship.attribute("Type").unwrap_or("").to_string(),
            target_mode: relationship
                .attribute("TargetMode")
                .unwrap_or("")
                .to_string(),
            resolved_part: resolve_rel_path(
                &path_dir(owner),
                target.split('#').next().unwrap_or(&target),
            ),
        });
    }
    // Excel stores the SmartArt render cache relationship on the worksheet drawing, while the
    // relationship id itself lives inside diagram data (`dsp:dataModelExt/@relId`) rather than
    // the graphic-frame anchor.  Dropping that apparently "unreferenced" relationship makes
    // Excel repair the drawing even though all four dgm:relIds are present.
    if kind == "smartart" {
        const DSP_NS: &str = "http://schemas.microsoft.com/office/drawing/2008/diagram";
        if let Some(data_part) = result
            .iter()
            .find(|relationship| relationship.role == "diagramData")
            .map(|relationship| relationship.resolved_part.clone())
        {
            if let Some(data_bytes) = snapshot.parts.get(&data_part) {
                let data_xml = std::str::from_utf8(data_bytes)
                    .map_err(|error| format!("SmartArt diagram data utf8: {error}"))?;
                let data_document = roxmltree::Document::parse(data_xml)
                    .map_err(|error| format!("SmartArt diagram data XML: {error}"))?;
                if let Some(cache_id) = data_document
                    .descendants()
                    .find(|node| {
                        node.is_element()
                            && node.tag_name().name() == "dataModelExt"
                            && node.tag_name().namespace() == Some(DSP_NS)
                            && node.attribute("relId").is_some()
                    })
                    .and_then(|node| node.attribute("relId"))
                {
                    if let Some(relationship) = document.descendants().find(|node| {
                        node.is_element()
                            && node.has_tag_name("Relationship")
                            && node.attribute("Id") == Some(cache_id)
                            && node
                                .attribute("Type")
                                .map(|value| value.ends_with("/diagramDrawing"))
                                .unwrap_or(false)
                    }) {
                        let target = relationship.attribute("Target").unwrap_or("").to_string();
                        result.push(NativeContentRelationship {
                            id: cache_id.to_string(),
                            role: "diagramDrawing".to_string(),
                            relationship_type: relationship
                                .attribute("Type")
                                .unwrap_or("")
                                .to_string(),
                            target_mode: relationship
                                .attribute("TargetMode")
                                .unwrap_or("")
                                .to_string(),
                            resolved_part: resolve_rel_path(
                                &path_dir(owner),
                                target.split('#').next().unwrap_or(&target),
                            ),
                        });
                    }
                }
            }
        }
    }
    Ok(result)
}

fn relative_opc_target(owner: &str, target: &str) -> String {
    let owner_directory = path_dir(owner);
    let from: Vec<&str> = owner_directory
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let to: Vec<&str> = target
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let mut common = 0usize;
    while common < from.len() && common < to.len() && from[common] == to[common] {
        common += 1;
    }
    let mut result = String::new();
    for _ in common..from.len() {
        result.push_str("../");
    }
    result.push_str(&to[common..].join("/"));
    result
}

fn allocate_native_content_part(
    snapshot: &OpcPackageSnapshot,
    parts: &std::collections::BTreeMap<String, Vec<u8>>,
    kind: &str,
) -> String {
    let (directory, stem) = match kind {
        "chart" => ("xl/charts", "unicellChart"),
        "diagramLayout" => ("xl/diagrams", "unicellLayout"),
        "diagramQuickStyle" => ("xl/diagrams", "unicellQuickStyle"),
        "diagramColors" => ("xl/diagrams", "unicellColors"),
        "drawing" | "diagramDrawing" => ("xl/drawings", "unicellNativeDrawing"),
        _ => ("xl/diagrams", "unicellData"),
    };
    for index in 1usize.. {
        let candidate = format!("{directory}/{stem}{index}.xml");
        if !snapshot.parts.contains_key(&candidate) && !parts.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn clone_native_content_part(
    snapshot: &OpcPackageSnapshot,
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    source_part: &str,
    kind: &str,
    copied: &mut std::collections::HashSet<String>,
    cloned_from: &mut std::collections::BTreeMap<String, String>,
) -> Result<String, String> {
    let source = snapshot
        .parts
        .get(source_part)
        .or_else(|| parts.get(source_part))
        .ok_or_else(|| format!("missing native {kind} content part {source_part}"))?
        .clone();
    let new_part = allocate_native_content_part(snapshot, parts, kind);
    parts.insert(new_part.clone(), source);
    copied.insert(new_part.clone());
    cloned_from.insert(new_part.clone(), source_part.to_string());

    let source_rels_path = drawing_relationships_path(source_part);
    if let Some(source_rels) = snapshot
        .parts
        .get(&source_rels_path)
        .or_else(|| parts.get(&source_rels_path))
        .cloned()
    {
        let relationships_xml = std::str::from_utf8(&source_rels)
            .map_err(|e| format!("native cloned part relationships utf8: {e}"))?;
        let document = roxmltree::Document::parse(relationships_xml)
            .map_err(|e| format!("native cloned part relationships XML: {e}"))?;
        let dependencies: Vec<(String, String, String)> = document
            .descendants()
            .filter(|node| {
                node.is_element()
                    && node.has_tag_name("Relationship")
                    && node.attribute("TargetMode") != Some("External")
            })
            .filter_map(|node| {
                Some((
                    node.attribute("Id")?.to_string(),
                    node.attribute("Type").unwrap_or("").to_string(),
                    node.attribute("Target")?.to_string(),
                ))
            })
            .collect();
        drop(document);
        let mut cloned_relationships = relationships_xml.to_string();
        for (id, relationship_type, target) in dependencies {
            let resolved = resolve_rel_path(
                &path_dir(source_part),
                target.split('#').next().unwrap_or(&target),
            );
            if relationship_type.ends_with("/chartUserShapes")
                || relationship_type.ends_with("/diagramDrawing")
            {
                let cloned_dependency = clone_owned_native_drawing_part(
                    snapshot,
                    parts,
                    &resolved,
                    copied,
                    cloned_from,
                )?;
                let new_target = relative_opc_target(&new_part, &cloned_dependency);
                cloned_relationships =
                    replace_relationship_target(&cloned_relationships, &id, &new_target)?;
            } else {
                copy_native_dependency_closure(
                    snapshot,
                    parts,
                    &resolved,
                    copied,
                    &mut std::collections::HashSet::new(),
                )?;
            }
        }
        let new_rels_path = drawing_relationships_path(&new_part);
        parts.insert(new_rels_path.clone(), cloned_relationships.into_bytes());
        copied.insert(new_rels_path);
    }
    Ok(new_part)
}

fn replace_relationship_target(
    relationships_xml: &str,
    id: &str,
    target: &str,
) -> Result<String, String> {
    let document = roxmltree::Document::parse(relationships_xml)
        .map_err(|error| format!("native cloned relationship XML: {error}"))?;
    let relationship = document
        .descendants()
        .find(|node| {
            node.is_element()
                && node.has_tag_name("Relationship")
                && node.attribute("Id") == Some(id)
        })
        .ok_or_else(|| format!("missing native cloned relationship {id}"))?;
    let range = relationship.range();
    let replacement = set_start_tag_attribute(&relationships_xml[range.clone()], "Target", target);
    drop(document);
    let mut result = relationships_xml.to_string();
    result.replace_range(range, &replacement);
    Ok(result)
}

fn clone_owned_native_drawing_part(
    snapshot: &OpcPackageSnapshot,
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    source_part: &str,
    copied: &mut std::collections::HashSet<String>,
    cloned_from: &mut std::collections::BTreeMap<String, String>,
) -> Result<String, String> {
    let source = snapshot
        .parts
        .get(source_part)
        .or_else(|| parts.get(source_part))
        .ok_or_else(|| format!("missing native owned drawing part {source_part}"))?
        .clone();
    let new_part = allocate_native_content_part(snapshot, parts, "drawing");
    parts.insert(new_part.clone(), source);
    copied.insert(new_part.clone());
    cloned_from.insert(new_part.clone(), source_part.to_string());
    let source_rels_path = drawing_relationships_path(source_part);
    if let Some(source_rels) = snapshot
        .parts
        .get(&source_rels_path)
        .or_else(|| parts.get(&source_rels_path))
        .cloned()
    {
        let relationships_xml = std::str::from_utf8(&source_rels)
            .map_err(|error| format!("native owned drawing relationships utf8: {error}"))?;
        let document = roxmltree::Document::parse(relationships_xml)
            .map_err(|error| format!("native owned drawing relationships XML: {error}"))?;
        let targets: Vec<String> = document
            .descendants()
            .filter(|node| {
                node.is_element()
                    && node.has_tag_name("Relationship")
                    && node.attribute("TargetMode") != Some("External")
            })
            .filter_map(|node| node.attribute("Target"))
            .map(|target| {
                resolve_rel_path(
                    &path_dir(source_part),
                    target.split('#').next().unwrap_or(target),
                )
            })
            .collect();
        drop(document);
        for target in targets {
            copy_native_dependency_closure(
                snapshot,
                parts,
                &target,
                copied,
                &mut std::collections::HashSet::new(),
            )?;
        }
        let new_rels_path = drawing_relationships_path(&new_part);
        parts.insert(new_rels_path.clone(), source_rels);
        copied.insert(new_rels_path);
    }
    Ok(new_part)
}

fn append_native_drawing_relationship(
    relationships_xml: &mut String,
    generated_owner: &str,
    relationship: &NativeContentRelationship,
    target_part: &str,
) -> Result<String, String> {
    let document = roxmltree::Document::parse(relationships_xml)
        .map_err(|e| format!("generated native drawing relationships XML: {e}"))?;
    let used: std::collections::HashSet<String> = document
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
        .filter_map(|node| node.attribute("Id").map(str::to_string))
        .collect();
    drop(document);
    let mut suffix = 1usize;
    let id = loop {
        // Excel's DrawingML loader is stricter than the OPC xsd:ID grammar here: arbitrary
        // NCNames such as rIdNativeClone1 make it repair the entire drawing part.  Allocate the
        // conventional numeric relationship form used by Office itself.
        let candidate = format!("rId{suffix}");
        suffix += 1;
        if !used.contains(&candidate) {
            break candidate;
        }
    };
    let target = relative_opc_target(generated_owner, target_part);
    let target_mode = if relationship.target_mode.is_empty() {
        String::new()
    } else {
        format!(" TargetMode=\"{}\"", html_escape(&relationship.target_mode))
    };
    let fragment = format!(
        "<Relationship Id=\"{}\" Type=\"{}\" Target=\"{}\"{}/>",
        html_escape(&id),
        html_escape(&relationship.relationship_type),
        html_escape(&target),
        target_mode,
    );
    let close = relationships_xml
        .rfind("</Relationships>")
        .ok_or("malformed generated native drawing relationships")?;
    relationships_xml.insert_str(close, &fragment);
    Ok(id)
}

fn native_descriptor_edit<'a>(descriptor: &'a Value, kind: &str) -> Option<&'a Value> {
    descriptor
        .get("edits")
        .and_then(|edits| edits.get(kind))
        .filter(|edit| {
            !edit
                .as_object()
                .map(|object| object.is_empty())
                .unwrap_or(false)
        })
}

fn apply_native_shape_anchor_edit(
    anchor: &str,
    source: &str,
    edit: &Value,
) -> Result<String, String> {
    let wrapped = drawing_fragment_document_with_source(anchor, Some(source));
    let updated = native_shape_edit::apply_shape_edit(&wrapped, edit)?;
    let document = roxmltree::Document::parse(&updated)
        .map_err(|error| format!("edited native shape XML: {error}"))?;
    let root = document.root_element();
    let Some(anchor_node) = root.children().find(|node| node.is_element()) else {
        return Err("edited native shape wrapper has no anchor".to_string());
    };
    Ok(updated[anchor_node.range()].to_string())
}

fn drawing_path_for_generated_sheet(
    parts: &std::collections::BTreeMap<String, Vec<u8>>,
    sheet: u32,
) -> Option<String> {
    let sheet_path = format!("xl/worksheets/sheet{}.xml", sheet + 1);
    let rels_path = drawing_relationships_path(&sheet_path);
    let rels = std::str::from_utf8(parts.get(&rels_path)?).ok()?;
    let document = roxmltree::Document::parse(rels).ok()?;
    let target = document
        .descendants()
        .find(|node| {
            node.is_element()
                && node.has_tag_name("Relationship")
                && node
                    .attribute("Type")
                    .map(|value| value.ends_with("/drawing"))
                    .unwrap_or(false)
                && node
                    .attribute("Target")
                    .map(|value| value.contains("unicellDrawing"))
                    .unwrap_or(false)
        })
        .or_else(|| {
            document.descendants().find(|node| {
                node.is_element()
                    && node.has_tag_name("Relationship")
                    && node
                        .attribute("Type")
                        .map(|value| value.ends_with("/drawing"))
                        .unwrap_or(false)
            })
        })?
        .attribute("Target")?;
    Some(resolve_rel_path(&path_dir(&sheet_path), target))
}

fn set_native_non_visual_id(fragment: &str, id: i64) -> String {
    for prefix in ["xdr", "a", "", "dsp"] {
        let updated =
            set_drawing_element_attributes(fragment, prefix, "cNvPr", &[("id", id.to_string())]);
        if updated != fragment {
            return updated;
        }
    }
    fragment.to_string()
}

fn native_clone_creation_id(seed: &str) -> String {
    let digest =
        sha256_hex(format!("unicell-native-drawing-clone:{seed}").as_bytes()).to_ascii_uppercase();
    format!(
        "{{{}-{}-{}-{}-{}}}",
        &digest[0..8],
        &digest[8..12],
        &digest[12..16],
        &digest[16..20],
        &digest[20..32]
    )
}

fn set_native_creation_id(fragment: &str, id: &str) -> String {
    for prefix in ["a16", "a14", "a", "", "dsp"] {
        let updated = set_drawing_element_attributes(
            fragment,
            prefix,
            "creationId",
            &[("id", id.to_string())],
        );
        if updated != fragment {
            return updated;
        }
    }
    fragment.to_string()
}

fn compose_native_drawings(
    st: &AppState,
    parts: &mut std::collections::BTreeMap<String, Vec<u8>>,
    snapshot: &OpcPackageSnapshot,
) -> Result<NativeCompositionResult, String> {
    let mut composition = NativeCompositionResult::default();
    let mut global_diagram_data_usage = std::collections::HashMap::<String, usize>::new();
    for object in st.objects.values().flat_map(|objects| objects.iter()) {
        let Some(descriptor) = object
            .get("config")
            .and_then(|config| config.get("nativeDrawing"))
        else {
            continue;
        };
        if descriptor["kind"] != "smartart" || descriptor["clone"].as_bool() == Some(true) {
            continue;
        }
        if let Some(data_part) = descriptor["contentPart"].as_str() {
            *global_diagram_data_usage
                .entry(data_part.to_string())
                .or_default() += 1;
        }
    }
    for (sheet, objects) in &st.objects {
        let native_objects: Vec<&Value> = objects
            .iter()
            .filter(|object| {
                object
                    .get("config")
                    .and_then(|value| value.get("nativeDrawing"))
                    .is_some()
            })
            .collect();
        if native_objects.is_empty() {
            continue;
        }
        let Some(generated_drawing_path) = drawing_path_for_generated_sheet(parts, *sheet) else {
            continue;
        };
        let Some(generated_bytes) = parts.get(&generated_drawing_path).cloned() else {
            continue;
        };
        let mut generated_xml = String::from_utf8(generated_bytes)
            .map_err(|e| format!("generated drawing utf8: {e}"))?;
        let generated_rels_path = drawing_relationships_path(&generated_drawing_path);
        let mut used_non_visual_ids: std::collections::HashSet<i64> =
            roxmltree::Document::parse(&generated_xml)
                .ok()
                .map(|document| {
                    document
                        .descendants()
                        .filter(|node| node.is_element() && node.tag_name().name() == "cNvPr")
                        .filter_map(|node| {
                            node.attribute("id")
                                .and_then(|value| value.parse::<i64>().ok())
                        })
                        .collect()
                })
                .unwrap_or_default();
        let mut next_non_visual_id = used_non_visual_ids.iter().copied().max().unwrap_or(0) + 1;
        let mut by_original_drawing: std::collections::BTreeMap<String, Vec<&Value>> =
            std::collections::BTreeMap::new();
        for object in native_objects {
            if let Some(path) = object["config"]["nativeDrawing"]["drawingPath"].as_str() {
                by_original_drawing
                    .entry(path.to_string())
                    .or_default()
                    .push(object);
            }
        }
        let mut native_fragments = String::new();
        for (original_drawing_path, drawing_objects) in by_original_drawing {
            let Some(original_bytes) = snapshot.parts.get(&original_drawing_path) else {
                continue;
            };
            let original_xml = std::str::from_utf8(original_bytes)
                .map_err(|e| format!("original drawing utf8: {e}"))?;
            merge_root_namespace_declarations(&mut generated_xml, original_xml);
            let anchors = drawing_anchor_slices(original_xml);
            let original_rels_path = drawing_relationships_path(&original_drawing_path);
            let original_rels = snapshot.parts.get(&original_rels_path);
            let original_rels_xml = original_rels
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .unwrap_or("");
            let mut selected: Vec<(String, &Value, Vec<NativeContentRelationship>, bool)> =
                Vec::new();
            let mut referenced_ids = std::collections::HashSet::new();
            for object in drawing_objects {
                let descriptor = &object["config"]["nativeDrawing"];
                let wanted_id = descriptor["nonVisualId"].as_str().unwrap_or("");
                let wanted_index = descriptor["anchorIndex"].as_u64().unwrap_or(u64::MAX) as usize;
                let found = anchors
                    .iter()
                    .enumerate()
                    .find(|(index, (start, end, _))| {
                        let raw = &original_xml[*start..*end];
                        (!wanted_id.is_empty()
                            && drawing_any_element_attr(raw, "cNvPr", "id").as_deref()
                                == Some(wanted_id))
                            || (wanted_id.is_empty() && *index == wanted_index)
                    })
                    .or_else(|| {
                        anchors
                            .get(wanted_index)
                            .map(|anchor| (wanted_index, anchor))
                    });
                let Some((_, (start, end, anchor_kind))) = found else {
                    continue;
                };
                let mut raw = original_xml[*start..*end].to_string();
                let kind = descriptor["kind"]
                    .as_str()
                    .unwrap_or_else(|| drawing_native_kind(&raw));
                let content_relationships = native_content_relationships(
                    &raw,
                    original_rels_xml,
                    &original_drawing_path,
                    kind,
                    snapshot,
                )?;
                let smartart_data_changed = if kind == "smartart" {
                    if let Some(edit) = native_descriptor_edit(descriptor, "smartart") {
                        let relationship = content_relationships
                            .iter()
                            .find(|relationship| relationship.role == "diagramData")
                            .ok_or_else(|| {
                                format!(
                                    "native smartart {} has no editable content relationship",
                                    descriptor["token"].as_str().unwrap_or("object")
                                )
                            })?;
                        if relationship.target_mode.eq_ignore_ascii_case("External") {
                            return Err(
                                "native smartart external content cannot be edited or cloned"
                                    .to_string(),
                            );
                        }
                        let original_content = snapshot
                            .parts
                            .get(&relationship.resolved_part)
                            .ok_or_else(|| {
                                format!(
                                    "missing native smartart content {}",
                                    relationship.resolved_part
                                )
                            })?;
                        let original_xml = std::str::from_utf8(original_content)
                            .map_err(|error| format!("native smartart utf8: {error}"))?;
                        let updated = native_smartart_edit::apply_smartart_edit(original_xml, edit)
                            .map_err(|error| {
                                format!(
                                    "native smartart {}: {error}",
                                    descriptor["token"].as_str().unwrap_or("object")
                                )
                            })?;
                        updated.as_bytes() != original_content
                    } else {
                        false
                    }
                } else {
                    false
                };
                if matches!(kind, "shape" | "connector" | "group") {
                    if let Some(edit) = native_descriptor_edit(descriptor, "shape") {
                        raw = apply_native_shape_anchor_edit(&raw, original_xml, edit).map_err(
                            |error| {
                                format!(
                                    "native shape {}: {error}",
                                    descriptor["token"].as_str().unwrap_or("object")
                                )
                            },
                        )?;
                    }
                }
                let current = native_object_absolute_geometry(st, *sheet, object);
                let baseline = &descriptor["baseline"];
                let baseline_geometry = (
                    json_number(baseline, "x", current.0),
                    json_number(baseline, "y", current.1),
                    json_number(baseline, "w", current.2),
                    json_number(baseline, "h", current.3),
                );
                let position_changed = (current.0 - baseline_geometry.0).abs() > 0.001
                    || (current.1 - baseline_geometry.1).abs() > 0.001;
                let size_changed = (current.2 - baseline_geometry.2).abs() > 0.001
                    || (current.3 - baseline_geometry.3).abs() > 0.001;
                let updated = if position_changed || size_changed {
                    rewrite_native_anchor_geometry(
                        &raw,
                        descriptor["anchorKind"].as_str().unwrap_or(anchor_kind),
                        &st.model,
                        *sheet,
                        current,
                        size_changed,
                    )
                } else {
                    raw
                };
                selected.push((
                    updated,
                    object,
                    content_relationships,
                    smartart_data_changed,
                ));
            }
            if !original_rels_xml.is_empty() {
                for (raw, object, content_relationships, smartart_data_changed) in &selected {
                    let descriptor = &object["config"]["nativeDrawing"];
                    let kind = descriptor["kind"]
                        .as_str()
                        .unwrap_or_else(|| drawing_native_kind(raw));
                    let is_clone = descriptor["clone"].as_bool() == Some(true);
                    let shared_edited_data = !is_clone
                        && kind == "smartart"
                        && *smartart_data_changed
                        && content_relationships
                            .iter()
                            .find(|relationship| relationship.role == "diagramData")
                            .and_then(|relationship| {
                                global_diagram_data_usage.get(&relationship.resolved_part)
                            })
                            .copied()
                            .unwrap_or(0)
                            > 1;
                    let mut ids = drawing_anchor_relationship_ids(raw, original_rels_xml)?;
                    if is_clone {
                        for relationship in content_relationships {
                            ids.remove(&relationship.id);
                        }
                    } else {
                        if shared_edited_data {
                            for relationship in content_relationships
                                .iter()
                                .filter(|relationship| relationship.role == "diagramData")
                            {
                                ids.remove(&relationship.id);
                            }
                        }
                        ids.extend(
                            content_relationships
                                .iter()
                                .filter(|relationship| {
                                    !(*smartart_data_changed
                                        && relationship.role == "diagramDrawing")
                                        && !(shared_edited_data
                                            && relationship.role == "diagramData")
                                })
                                .map(|relationship| relationship.id.clone()),
                        );
                    }
                    referenced_ids.extend(ids);
                }
            }
            let mut id_map = std::collections::HashMap::new();
            if let Some(original_rels) = original_rels {
                let generated_rels = parts.get(&generated_rels_path).cloned();
                let (merged, map) = merge_selected_drawing_relationships(
                    generated_rels.as_deref(),
                    original_rels,
                    &original_drawing_path,
                    &referenced_ids,
                    snapshot,
                    parts,
                    &mut composition.parts,
                )?;
                parts.insert(generated_rels_path.clone(), merged);
                id_map = map;
            }
            for (raw, object, content_relationships, smartart_data_changed) in selected {
                let descriptor = &object["config"]["nativeDrawing"];
                let kind = descriptor["kind"]
                    .as_str()
                    .unwrap_or_else(|| drawing_native_kind(&raw));
                let mut local_id_map = id_map.clone();
                let is_clone = descriptor["clone"].as_bool() == Some(true);
                let mut cloned_smartart_data_part: Option<String> = None;
                if matches!(kind, "chart" | "smartart") {
                    let edit_key = if kind == "chart" { "chart" } else { "smartart" };
                    let edit = native_descriptor_edit(descriptor, edit_key);
                    if edit.is_some() || is_clone {
                        let editable_role = if kind == "chart" {
                            "chart"
                        } else {
                            "diagramData"
                        };
                        let relationship = content_relationships
                            .iter()
                            .find(|relationship| relationship.role == editable_role)
                            .ok_or_else(|| {
                                format!(
                                    "native {kind} {} has no editable content relationship",
                                    descriptor["token"].as_str().unwrap_or("object")
                                )
                            })?;
                        if relationship.target_mode.eq_ignore_ascii_case("External") {
                            return Err(format!(
                                "native {kind} external content cannot be edited or cloned"
                            ));
                        }
                        let fork_shared_smartart_data = kind == "smartart"
                            && !is_clone
                            && smartart_data_changed
                            && global_diagram_data_usage
                                .get(&relationship.resolved_part)
                                .copied()
                                .unwrap_or(0)
                                > 1;
                        if fork_shared_smartart_data {
                            let smartart_edit = edit.ok_or_else(|| {
                                "changed shared SmartArt data has no edit payload".to_string()
                            })?;
                            let cloned_part = clone_native_content_part(
                                snapshot,
                                parts,
                                &relationship.resolved_part,
                                &relationship.role,
                                &mut composition.parts,
                                &mut composition.cloned_from,
                            )?;
                            let original_content = parts.get(&cloned_part).ok_or_else(|| {
                                format!("missing cloned native content {cloned_part}")
                            })?;
                            let original_content = std::str::from_utf8(original_content)
                                .map_err(|error| format!("cloned native {kind} utf8: {error}"))?;
                            let updated = native_smartart_edit::apply_smartart_edit(
                                original_content,
                                smartart_edit,
                            )
                            .map_err(|error| {
                                format!(
                                    "native {kind} {}: {error}",
                                    descriptor["token"].as_str().unwrap_or("object")
                                )
                            })?;
                            parts.insert(cloned_part.clone(), updated.into_bytes());
                            let mut generated_relationships = parts
                                .get(&generated_rels_path)
                                .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                                .unwrap_or_else(|| "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_string());
                            let new_id = append_native_drawing_relationship(
                                &mut generated_relationships,
                                &generated_drawing_path,
                                relationship,
                                &cloned_part,
                            )?;
                            parts.insert(
                                generated_rels_path.clone(),
                                generated_relationships.into_bytes(),
                            );
                            local_id_map.insert(relationship.id.clone(), new_id);
                            cloned_smartart_data_part = Some(cloned_part);
                        } else if is_clone {
                            let mut generated_relationships = parts.get(&generated_rels_path)
                                .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                                .unwrap_or_else(|| "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_string());
                            for cloned_relationship in &content_relationships {
                                if cloned_relationship
                                    .target_mode
                                    .eq_ignore_ascii_case("External")
                                {
                                    return Err(format!(
                                        "native {kind} external content cannot be cloned"
                                    ));
                                }
                                if kind == "smartart"
                                    && smartart_data_changed
                                    && cloned_relationship.role == "diagramDrawing"
                                {
                                    // The cache is derived from logical SmartArt data.  Keeping a
                                    // byte-for-byte clone after node edits makes Office reject the
                                    // graphic frame; omitting it lets Office regenerate a native
                                    // editable cache on first layout.
                                    continue;
                                }
                                let cloned_part = clone_native_content_part(
                                    snapshot,
                                    parts,
                                    &cloned_relationship.resolved_part,
                                    &cloned_relationship.role,
                                    &mut composition.parts,
                                    &mut composition.cloned_from,
                                )?;
                                if kind == "smartart" && cloned_relationship.role == "diagramData" {
                                    cloned_smartart_data_part = Some(cloned_part.clone());
                                }
                                if cloned_relationship.role == editable_role {
                                    if let Some(edit) = edit {
                                        let original_content =
                                            parts.get(&cloned_part).ok_or_else(|| {
                                                format!(
                                                    "missing cloned native content {cloned_part}"
                                                )
                                            })?;
                                        let original_content = std::str::from_utf8(
                                            original_content,
                                        )
                                        .map_err(|error| {
                                            format!("cloned native {kind} utf8: {error}")
                                        })?;
                                        let updated = if kind == "chart" {
                                            native_chart_edit::apply_chart_edit(
                                                original_content,
                                                edit,
                                            )
                                        } else {
                                            native_smartart_edit::apply_smartart_edit(
                                                original_content,
                                                edit,
                                            )
                                        }
                                        .map_err(|error| {
                                            format!(
                                                "native {kind} {}: {error}",
                                                descriptor["token"].as_str().unwrap_or("object")
                                            )
                                        })?;
                                        parts.insert(cloned_part.clone(), updated.into_bytes());
                                    }
                                }
                                let new_id = append_native_drawing_relationship(
                                    &mut generated_relationships,
                                    &generated_drawing_path,
                                    cloned_relationship,
                                    &cloned_part,
                                )?;
                                local_id_map.insert(cloned_relationship.id.clone(), new_id);
                            }
                            parts.insert(
                                generated_rels_path.clone(),
                                generated_relationships.into_bytes(),
                            );
                        } else if let Some(edit) = edit {
                            let original_content = snapshot
                                .parts
                                .get(&relationship.resolved_part)
                                .or_else(|| parts.get(&relationship.resolved_part))
                                .ok_or_else(|| {
                                    format!(
                                        "missing native {kind} content {}",
                                        relationship.resolved_part
                                    )
                                })?;
                            let original_content = std::str::from_utf8(original_content)
                                .map_err(|error| format!("native {kind} utf8: {error}"))?;
                            let updated = if kind == "chart" {
                                native_chart_edit::apply_chart_edit(original_content, edit)
                            } else {
                                native_smartart_edit::apply_smartart_edit(original_content, edit)
                            }
                            .map_err(|error| {
                                format!(
                                    "native {kind} {}: {error}",
                                    descriptor["token"].as_str().unwrap_or("object")
                                )
                            })?;
                            parts.insert(relationship.resolved_part.clone(), updated.into_bytes());
                            composition.parts.insert(relationship.resolved_part.clone());
                        }
                    }
                    if kind == "smartart" {
                        let data_part = cloned_smartart_data_part.or_else(|| {
                            if is_clone {
                                None
                            } else {
                                content_relationships
                                    .iter()
                                    .find(|relationship| relationship.role == "diagramData")
                                    .map(|relationship| relationship.resolved_part.clone())
                            }
                        });
                        if let Some(data_part) = data_part {
                            if let Some(bytes) = parts
                                .get(&data_part)
                                .or_else(|| snapshot.parts.get(&data_part))
                                .cloned()
                            {
                                let xml = String::from_utf8(bytes).map_err(|error| {
                                    format!("SmartArt diagram data utf8: {error}")
                                })?;
                                let xml = if smartart_data_changed {
                                    strip_smartart_diagram_cache_extension(&xml)?
                                } else {
                                    xml
                                };
                                parts.insert(
                                    data_part,
                                    remap_diagram_data_relationship(xml, &local_id_map)
                                        .into_bytes(),
                                );
                            }
                        }
                    }
                }
                let mut fragment = remap_drawing_relationship_attributes(raw, &local_id_map);
                if is_clone {
                    let seed = descriptor["token"]
                        .as_str()
                        .or_else(|| object["id"].as_str())
                        .unwrap_or("native-clone");
                    fragment = set_native_creation_id(&fragment, &native_clone_creation_id(seed));
                }
                let original_id = drawing_any_element_attr(&fragment, "cNvPr", "id")
                    .and_then(|value| value.parse::<i64>().ok());
                let id = match original_id {
                    Some(value) if !used_non_visual_ids.contains(&value) => value,
                    _ => {
                        while used_non_visual_ids.contains(&next_non_visual_id) {
                            next_non_visual_id += 1;
                        }
                        let value = next_non_visual_id;
                        next_non_visual_id += 1;
                        fragment = set_native_non_visual_id(&fragment, value);
                        value
                    }
                };
                used_non_visual_ids.insert(id);
                next_non_visual_id = next_non_visual_id.max(id.saturating_add(1));
                native_fragments.push_str(&fragment);
            }
        }
        if !native_fragments.is_empty() {
            let document = roxmltree::Document::parse(&generated_xml)
                .map_err(|e| format!("generated drawing XML: {e}"))?;
            let root_start = document.root_element().range().start;
            let insert_at = root_start
                + generated_xml[root_start..]
                    .find('>')
                    .ok_or("malformed generated drawing root")?
                + 1;
            drop(document);
            generated_xml.insert_str(insert_at, &native_fragments);
            parts.insert(generated_drawing_path, generated_xml.into_bytes());
        }
    }
    Ok(composition)
}

fn restore_preserved_ooxml(st: &AppState, xlsx: Vec<u8>) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Read, Write};
    let Some(snapshot) = st.source_ooxml.as_ref() else {
        if st.native_data_edits.is_empty()
            && st.native_pivot_local_refresh_edits.is_empty()
            && st.native_table_edits.is_empty()
            && st.native_page_review_edits.is_empty()
        {
            return Ok(xlsx);
        }
        let mut parts = snapshot_opc_package(&xlsx)?.parts;
        apply_native_data_edit_journal(&mut parts, &st.native_data_edits)?;
        apply_table_edit_journal(&mut parts, &st.native_table_edits)?;
        apply_pivot_local_refresh_journal(&mut parts, &st.native_pivot_local_refresh_edits)?;
        apply_page_review_edit_journal(&mut parts, &st.native_page_review_edits)?;
        return encode_opc_parts(parts);
    };
    let mut archive =
        zip::read::ZipArchive::new(Cursor::new(xlsx)).map_err(|e| format!("generated OPC: {e}"))?;
    let mut parts: std::collections::BTreeMap<String, Vec<u8>> = std::collections::BTreeMap::new();
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| format!("generated OPC entry: {e}"))?;
        if file.is_dir() {
            continue;
        }
        let Some(name) = normalize_opc_part_name(file.name()) else {
            continue;
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("generated OPC read {name}: {e}"))?;
        parts.insert(name, bytes);
    }
    drop(archive);

    if snapshot_workbook_theme_part(snapshot).is_some() {
        parts.retain(|name, _| !name.starts_with("xl/theme/") || snapshot.parts.contains_key(name));
    }
    for (name, bytes) in &snapshot.parts {
        if opc_part_is_preservable(name) {
            parts.insert(name.clone(), bytes.clone());
        }
    }
    // Apply refresh-only PivotCache edits after the original preservable part has been restored,
    // otherwise the snapshot copy above would silently overwrite the user's change.
    for (part, patch) in &st.pivot_cache_refresh_edits {
        let bytes = parts
            .get(part)
            .ok_or_else(|| format!("edited pivot cache part {part} is unavailable"))?;
        let xml = std::str::from_utf8(bytes)
            .map_err(|error| format!("edited pivot cache UTF-8: {error}"))?;
        let edited = native_pivot_cache_edit::apply_pivot_cache_refresh_patch(xml, patch)?;
        parts.insert(part.clone(), edited.into_bytes());
    }

    let mut rel_id_maps: std::collections::HashMap<
        String,
        std::collections::HashMap<String, String>,
    > = std::collections::HashMap::new();
    for (rels_path, original_rels) in snapshot
        .parts
        .iter()
        .filter(|(name, _)| name.ends_with(".rels"))
    {
        let Some(owner) = opc_relationship_owner(rels_path) else {
            continue;
        };
        if owner.starts_with("_xmlsignatures/") {
            continue;
        }
        if owner.is_empty() || opc_part_is_controlled(&owner) {
            if !owner.is_empty() && !parts.contains_key(&owner) {
                continue;
            }
            let generated_rels = parts.get(rels_path).map(Vec::as_slice);
            let (merged, id_map) =
                merge_relationship_xml(generated_rels, original_rels, &owner, snapshot)?;
            parts.insert(rels_path.clone(), merged);
            rel_id_maps.insert(rels_path.clone(), id_map);
        } else if opc_part_is_preservable(&owner) && parts.contains_key(&owner) {
            parts.insert(rels_path.clone(), original_rels.clone());
        }
    }

    const WORKBOOK_ORDER: &[&str] = &[
        "fileVersion",
        "fileSharing",
        "workbookPr",
        "workbookProtection",
        "bookViews",
        "sheets",
        "functionGroups",
        "externalReferences",
        "definedNames",
        "calcPr",
        "oleSize",
        "customWorkbookViews",
        "pivotCaches",
        "smartTagPr",
        "smartTagTypes",
        "webPublishing",
        "fileRecoveryPr",
        "webPublishObjects",
        "extLst",
    ];
    const WORKBOOK_PRESERVE: &[&str] = &[
        "fileSharing",
        "workbookProtection",
        "functionGroups",
        "externalReferences",
        "calcPr",
        "oleSize",
        "customWorkbookViews",
        "pivotCaches",
        "smartTagPr",
        "smartTagTypes",
        "webPublishing",
        "fileRecoveryPr",
        "webPublishObjects",
        "extLst",
    ];
    if let (Some(generated), Some(original)) = (
        parts.get("xl/workbook.xml").cloned(),
        snapshot.parts.get("xl/workbook.xml"),
    ) {
        let mut xml = String::from_utf8(generated).map_err(|e| format!("workbook utf8: {e}"))?;
        let original_xml =
            std::str::from_utf8(original).map_err(|e| format!("original workbook utf8: {e}"))?;
        let preserve: std::collections::HashSet<&str> = WORKBOOK_PRESERVE.iter().copied().collect();
        let id_map = rel_id_maps
            .get("xl/_rels/workbook.xml.rels")
            .cloned()
            .unwrap_or_default();
        // calcPr is a controlled semantic block, but its vendor/forward-compatible attributes
        // must survive. Restore the exact imported node first; the final calculation-settings
        // pass changes only calcMode/iterate/iterateCount/iterateDelta when the user edited them.
        replace_existing_top_level_block(&mut xml, original_xml, "calcPr")?;
        merge_top_level_blocks(&mut xml, original_xml, WORKBOOK_ORDER, &preserve, &id_map)?;
        parts.insert("xl/workbook.xml".to_string(), xml.into_bytes());
    }

    const SHEET_ORDER: &[&str] = &[
        "sheetPr",
        "dimension",
        "sheetViews",
        "sheetFormatPr",
        "cols",
        "sheetData",
        "sheetCalcPr",
        "sheetProtection",
        "protectedRanges",
        "scenarios",
        "autoFilter",
        "sortState",
        "dataConsolidate",
        "customSheetViews",
        "mergeCells",
        "phoneticPr",
        "conditionalFormatting",
        "dataValidations",
        "hyperlinks",
        "printOptions",
        "pageMargins",
        "pageSetup",
        "headerFooter",
        "rowBreaks",
        "colBreaks",
        "customProperties",
        "cellWatches",
        "ignoredErrors",
        "smartTags",
        "drawing",
        "legacyDrawing",
        "legacyDrawingHF",
        "picture",
        "oleObjects",
        "controls",
        "webPublishItems",
        "tableParts",
        "pivotTableParts",
        "extLst",
    ];
    const SHEET_PRESERVE: &[&str] = &[
        "sheetProtection",
        "protectedRanges",
        "scenarios",
        "autoFilter",
        "sortState",
        "dataConsolidate",
        "customSheetViews",
        "phoneticPr",
        "hyperlinks",
        "printOptions",
        "pageMargins",
        "pageSetup",
        "headerFooter",
        "rowBreaks",
        "colBreaks",
        "customProperties",
        "cellWatches",
        "ignoredErrors",
        "smartTags",
        "legacyDrawing",
        "legacyDrawingHF",
        "picture",
        "oleObjects",
        "controls",
        "webPublishItems",
        "tableParts",
        "pivotTableParts",
    ];
    let preserve: std::collections::HashSet<&str> = SHEET_PRESERVE.iter().copied().collect();
    let sheet_paths: Vec<String> = parts
        .keys()
        .filter(|name| {
            name.starts_with("xl/worksheets/sheet")
                && name.ends_with(".xml")
                && !name.contains("/_rels/")
        })
        .cloned()
        .collect();
    for sheet_path in sheet_paths {
        let Some(original) = snapshot.parts.get(&sheet_path) else {
            continue;
        };
        let Some(generated) = parts.get(&sheet_path).cloned() else {
            continue;
        };
        let mut xml = String::from_utf8(generated).map_err(|e| format!("sheet utf8: {e}"))?;
        restore_formula_transport_for_sheet(st, &sheet_path, &mut xml)?;
        let original_xml =
            std::str::from_utf8(original).map_err(|e| format!("original sheet utf8: {e}"))?;
        let file = sheet_path.rsplit('/').next().unwrap_or("");
        let rels_path = format!("xl/worksheets/_rels/{file}.rels");
        let id_map = rel_id_maps.get(&rels_path).cloned().unwrap_or_default();
        merge_top_level_blocks(&mut xml, original_xml, SHEET_ORDER, &preserve, &id_map)?;
        let sheet_index = file
            .strip_prefix("sheet")
            .and_then(|value| value.strip_suffix(".xml"))
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        restore_worksheet_feature_subtrees(st, sheet_index, &mut xml, original_xml)?;
        parts.insert(sheet_path, xml.into_bytes());
    }

    if let (Some(generated), Some(original)) = (
        parts.get("xl/styles.xml").cloned(),
        snapshot.parts.get("xl/styles.xml"),
    ) {
        parts.insert(
            "xl/styles.xml".to_string(),
            merge_original_dxfs(&generated, original)?,
        );
    }

    let native_parts = compose_native_drawings(st, &mut parts, snapshot)?;

    if let (Some(generated), Some(original)) = (
        parts.get("[Content_Types].xml").cloned(),
        snapshot.parts.get("[Content_Types].xml"),
    ) {
        parts.insert(
            "[Content_Types].xml".to_string(),
            merge_content_types(
                &generated,
                original,
                snapshot,
                &native_parts.parts,
                &native_parts.cloned_from,
            )?,
        );
    }

    // Deep native feature edits run only after workbook/worksheet relationships, extension
    // blocks and DrawingML anchors have been composed.  This lets a cache/view rename or delete
    // cascade through the final package instead of being overwritten by preservation later.
    apply_native_data_edit_journal(&mut parts, &st.native_data_edits)?;
    apply_table_edit_journal(&mut parts, &st.native_table_edits)?;
    apply_pivot_local_refresh_journal(&mut parts, &st.native_pivot_local_refresh_edits)?;
    apply_pivot_table_edit_journal(&mut parts, &st.native_pivot_table_edits)?;
    apply_slicer_edit_journal(&mut parts, &st.native_slicer_edits)?;
    apply_timeline_edit_journal(&mut parts, &st.native_timeline_edits)?;
    apply_page_review_edit_journal(&mut parts, &st.native_page_review_edits)?;

    let mut writer = zip::write::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in parts {
        writer
            .start_file(name, options)
            .map_err(|e| format!("OPC write: {e}"))?;
        writer
            .write_all(&bytes)
            .map_err(|e| format!("OPC write data: {e}"))?;
    }
    Ok(writer
        .finish()
        .map_err(|e| format!("OPC finish: {e}"))?
        .into_inner())
}

fn apply_typed_data_validations(st: &AppState, xlsx: Vec<u8>) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Read, Write};
    if st.worksheet_features.data_validation_dirty.is_empty() {
        return Ok(xlsx);
    }
    let mut archive = zip::read::ZipArchive::new(Cursor::new(xlsx))
        .map_err(|e| format!("validation OPC: {e}"))?;
    let mut parts = std::collections::BTreeMap::new();
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| format!("validation OPC entry: {e}"))?;
        if file.is_dir() {
            continue;
        }
        let Some(name) = normalize_opc_part_name(file.name()) else {
            continue;
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("validation OPC read {name}: {e}"))?;
        parts.insert(name, bytes);
    }
    drop(archive);

    let mut path_lookup = std::collections::HashMap::new();
    for name in ["xl/workbook.xml", "xl/_rels/workbook.xml.rels"] {
        if let Some(bytes) = parts.get(name) {
            path_lookup.insert(name.to_string(), bytes.clone());
        }
    }
    for name in parts.keys().filter(|name| {
        name.starts_with("xl/worksheets/") && name.ends_with(".xml") && !name.contains("/_rels/")
    }) {
        path_lookup.entry(name.clone()).or_default();
    }
    let sheet_paths = workbook_sheet_paths(&path_lookup);
    for sheet in &st.worksheet_features.data_validation_dirty {
        let path = sheet_paths
            .get(*sheet as usize)
            .ok_or_else(|| format!("data validation sheet {sheet} is unavailable"))?;
        let bytes = parts
            .get(path)
            .ok_or_else(|| format!("data validation part {path} is unavailable"))?;
        let mut xml = std::str::from_utf8(bytes)
            .map_err(|e| format!("data validation worksheet utf8: {e}"))?
            .to_string();
        let empty = DataValidationSheet::default();
        let transport = st
            .worksheet_features
            .data_validations
            .get(sheet)
            .unwrap_or(&empty);
        apply_data_validation_sheet(&mut xml, transport)?;
        parts.insert(path.clone(), xml.into_bytes());
    }

    let mut writer = zip::write::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in parts {
        writer
            .start_file(name, options)
            .map_err(|e| format!("validation OPC write: {e}"))?;
        writer
            .write_all(&bytes)
            .map_err(|e| format!("validation OPC write data: {e}"))?;
    }
    Ok(writer
        .finish()
        .map_err(|e| format!("validation OPC finish: {e}"))?
        .into_inner())
}

fn patch_calculation_properties_xml(
    xml: &mut String,
    mode: CalculationMode,
    settings: &IterationSettings,
) -> Result<(), String> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| format!("calculation workbook XML: {error}"))?;
    let root = document.root_element();
    let root_name = xml_opening_qualified_name(&xml[root.range()])
        .ok_or("calculation workbook root has no qualified name")?;
    let calc = root
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "calcPr");
    let existing_start = calc.map(|node| node.range().start);
    let insert_at = if existing_start.is_none() {
        const AFTER_CALC_PR: &[&str] = &[
            "oleSize",
            "customWorkbookViews",
            "pivotCaches",
            "smartTagPr",
            "smartTagTypes",
            "webPublishing",
            "fileRecoveryPr",
            "webPublishObjects",
            "extLst",
        ];
        root.children()
            .find(|node| node.is_element() && AFTER_CALC_PR.contains(&node.tag_name().name()))
            .map(|node| node.range().start)
            .or_else(|| xml.rfind(&format!("</{root_name}>")))
            .ok_or("calculation workbook root has no closing tag")?
    } else {
        0
    };
    drop(document);

    let mut tag = if let Some(start) = existing_start {
        let relative_end = xml_start_tag_end(&xml[start..])
            .ok_or("calculation properties have an unterminated start tag")?;
        xml[start..=start + relative_end].to_string()
    } else {
        format!("<{}/>", xml_qualified_name_with_local(&root_name, "calcPr"))
    };
    tag = set_start_tag_attribute(&tag, "calcMode", mode.as_str());
    tag = set_start_tag_attribute(&tag, "iterate", if settings.enabled { "1" } else { "0" });
    tag = set_start_tag_attribute(
        &tag,
        "iterateCount",
        &settings.maximum_iterations.to_string(),
    );
    tag = set_start_tag_attribute(&tag, "iterateDelta", &settings.maximum_change.to_string());
    if let Some(start) = existing_start {
        let relative_end = xml_start_tag_end(&xml[start..]).unwrap();
        xml.replace_range(start..=start + relative_end, &tag);
    } else {
        xml.insert_str(insert_at, &tag);
    }
    Ok(())
}

fn apply_calculation_properties(st: &AppState, xlsx: Vec<u8>) -> Result<Vec<u8>, String> {
    if !st.calculation_properties_dirty {
        return Ok(xlsx);
    }
    let mut parts = snapshot_opc_package(&xlsx)?.parts;
    let workbook = parts
        .get("xl/workbook.xml")
        .ok_or("calculation export has no xl/workbook.xml")?;
    let mut xml = String::from_utf8(workbook.clone())
        .map_err(|error| format!("calculation workbook UTF-8: {error}"))?;
    patch_calculation_properties_xml(
        &mut xml,
        st.calculation_mode,
        &st.model.get_iteration_settings(),
    )?;
    parts.insert("xl/workbook.xml".to_string(), xml.into_bytes());
    encode_opc_parts(parts)
}

fn model_to_preserved_xlsx_bytes(st: &AppState) -> Result<Vec<u8>, String> {
    let bytes = restore_preserved_ooxml(st, model_to_xlsx_bytes(st)?)?;
    let bytes = apply_typed_data_validations(st, bytes)?;
    let bytes = apply_what_if_scenarios(st, bytes)?;
    apply_calculation_properties(st, bytes)
}

fn model_to_xlsx_bytes(st: &AppState) -> Result<Vec<u8>, String> {
    let tmp = unique_xlsx_temp_path("export");
    let saved = save_to_xlsx(st.model.get_model(), tmp.to_str().ok_or("bad temp path")?)
        .map_err(|e| format!("xlsx save: {e}"));
    if let Err(error) = saved {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    let bytes = std::fs::read(&tmp).map_err(|e| format!("read: {e}"));
    let _ = std::fs::remove_file(&tmp);
    let bytes = bytes?;
    inject_rich_shared_strings(bytes, &st.rich_text_xml)
}

fn enrich_export_objects_with_native_drawings(
    st: &AppState,
    mut objects: Vec<Value>,
) -> Vec<Value> {
    let mut positions: std::collections::HashMap<String, usize> = objects
        .iter()
        .enumerate()
        .filter_map(|(index, object)| object["id"].as_str().map(|id| (id.to_string(), index)))
        .collect();
    for state_object in st.objects.values().flat_map(|items| items.iter()) {
        let Some(native) = state_object
            .get("config")
            .and_then(|v| v.get("nativeDrawing"))
        else {
            continue;
        };
        let Some(id) = state_object["id"].as_str() else {
            continue;
        };
        if let Some(index) = positions.get(id).copied() {
            if let Some(map) = objects[index].as_object_mut() {
                map.insert("nativeDrawing".to_string(), native.clone());
            }
            continue;
        }
        let object = json!({
            "id": id,
            "type": state_object["type"].clone(),
            "mode": state_object["mode"].clone(),
            "sheet": state_object["sheet"].clone(),
            "r": state_object["r"].clone(),
            "c": state_object["c"].clone(),
            "x": state_object["x"].clone(),
            "y": state_object["y"].clone(),
            "w": state_object["w"].clone(),
            "h": state_object["h"].clone(),
            "nativeDrawing": native.clone(),
        });
        positions.insert(id.to_string(), objects.len());
        objects.push(object);
    }
    objects
}

fn api_export(st: &AppState, query: &str, body: &[u8]) -> Result<Resp, String> {
    let bytes = model_to_xlsx_bytes(st)?;
    // 插入对象嵌图（SVG→EMF、图片/截图→PNG）注入 drawing 图层
    let objects: Vec<Value> = if !body.is_empty() {
        parse_body(body)?
            .get("objects")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let objects = enrich_export_objects_with_native_drawings(st, objects);
    let equations = build_equations(st);
    let bytes = if objects.is_empty() && equations.is_empty() {
        bytes
    } else {
        inject_drawings(bytes, &objects, &equations)?
    };
    let bytes = restore_preserved_ooxml(st, bytes)?;
    let bytes = apply_typed_data_validations(st, bytes)?;
    let bytes = apply_what_if_scenarios(st, bytes)?;
    let bytes = apply_calculation_properties(st, bytes)?;
    let basename = export_basename(st, query);
    let fname = http_safe_attachment_name(&basename, &st.excel_extension);
    Ok(tiny_http::Response::from_data(bytes)
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], st.excel_mime.as_bytes()).unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Disposition"[..], fname.as_bytes()).unwrap(),
        ))
}

fn apply_what_if_scenarios(st: &AppState, bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if !st.what_if_scenarios.is_dirty() {
        return Ok(bytes);
    }
    let mut package = snapshot_opc_package(&bytes)?;
    let sheet_parts = snapshot_workbook_sheet_paths(&package);
    what_if_runtime::apply_scenarios_to_parts(
        &mut package.parts,
        &sheet_parts,
        &st.what_if_scenarios,
    )?;
    encode_opc_parts(package.parts)
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    fn response_value(response: Resp) -> Value {
        assert!(response.status_code().0 < 400);
        let mut bytes = Vec::new();
        response.into_reader().read_to_end(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn font_repository_paths_mime_and_cache_policy_are_strict() {
        assert!(valid_relative_asset_path("arial.ttf"));
        assert!(valid_relative_asset_path("collections/cambria.ttc"));
        for unsafe_path in [
            "",
            "../arial.ttf",
            "/arial.ttf",
            "C:/arial.ttf",
            "a\\b.ttf",
            "a\0b.ttf",
        ] {
            assert!(
                !valid_relative_asset_path(unsafe_path),
                "accepted {unsafe_path:?}"
            );
        }

        assert_eq!(
            static_content_type(std::path::Path::new("arial.ttf")),
            "font/ttf"
        );
        assert_eq!(
            static_content_type(std::path::Path::new("cambria.ttc")),
            "font/collection"
        );
        assert_eq!(
            static_content_type(std::path::Path::new("manifest.json")),
            "application/json; charset=utf-8"
        );

        let bytes = b"verified-font-content";
        let hash = format!("{:x}", sha2::Sha256::digest(bytes));
        assert_eq!(
            verified_font_cache_control(
                &format!("v={hash}"),
                std::path::Path::new("arial.ttf"),
                bytes
            ),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            verified_font_cache_control("v=deadbeef", std::path::Path::new("arial.ttf"), bytes),
            "no-cache, must-revalidate"
        );
        assert_eq!(
            verified_font_cache_control(
                &format!("v={hash}"),
                std::path::Path::new("manifest.json"),
                bytes
            ),
            "no-cache, must-revalidate"
        );
    }

    #[test]
    fn csv_import_export_handles_excel_encodings_quotes_newlines_and_formula_values() {
        let mut state = AppState::new();
        let source = "\u{feff}姓名,备注,数值,公式,大整数,编号\r\n小明,\"含,逗号\n和换行\",7,=3*4,932321676362776576.000,08123\r\n";
        let response = api_import_csv(&mut state, source.as_bytes()).unwrap();
        let imported = response_value(response);
        assert_eq!(imported["rows"], 2);
        assert_eq!(imported["columns"], 6);
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "小明");
        assert_eq!(
            state.model.get_cell_content(0, 2, 2).unwrap(),
            "含,逗号\n和换行"
        );
        assert_eq!(state.model.get_cell_content(0, 2, 4).unwrap(), "=3*4");
        assert_eq!(state.model.get_formatted_cell_value(0, 2, 4).unwrap(), "12");
        assert_eq!(
            state.model.get_cell_content(0, 2, 5).unwrap(),
            "932321676362776576.000"
        );
        assert_eq!(state.model.get_cell_content(0, 2, 6).unwrap(), "08123");

        let response = api_export_csv(&state, "name=中文数据&sheet=0").unwrap();
        let mut bytes = Vec::new();
        response.into_reader().read_to_end(&mut bytes).unwrap();
        assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
        let exported = std::str::from_utf8(&bytes[3..]).unwrap();
        assert!(
            exported.contains("小明,\"含,逗号\n和换行\",7,12,932321676362776576.000,08123\r\n")
        );
        assert!(!exported.contains("=3*4"));

        let (gbk, _, had_errors) = encoding_rs::GBK.encode("名称,城市\r\n测试,深圳\r\n");
        assert!(!had_errors);
        let mut gbk_state = AppState::new();
        api_import_csv(&mut gbk_state, &gbk).unwrap();
        assert_eq!(gbk_state.model.get_cell_content(0, 2, 2).unwrap(), "深圳");
    }

    #[test]
    fn session_store_keeps_workbooks_private_and_preserves_cookie_continuity() {
        let mut store = SessionStore::default();
        let session_a = store.resolve(None).unwrap();
        assert!(session_a.set_cookie);
        assert!(valid_session_id(&session_a.id));

        let same_a = store.resolve(Some(&session_a.id)).unwrap();
        assert_eq!(same_a.id, session_a.id);
        assert!(!same_a.set_cookie);

        let session_b = store.resolve(None).unwrap();
        assert_ne!(session_a.id, session_b.id);
        let attacker_chosen = "cd".repeat(SESSION_ID_BYTES);
        let replacement = store.resolve(Some(&attacker_chosen)).unwrap();
        assert!(replacement.set_cookie);
        assert_ne!(replacement.id, attacker_chosen);
        let scope_a = store
            .state_mut(&session_a.id)
            .unwrap()
            .storage_scope
            .clone();
        let scope_b = store
            .state_mut(&session_b.id)
            .unwrap()
            .storage_scope
            .clone();
        assert_ne!(scope_a, scope_b);

        let secret = "A_ONLY_1d35b26e";
        let body = serde_json::to_vec(&json!({
            "sheet": 0, "row": 20, "col": 1, "value": secret,
        }))
        .unwrap();
        handle_api_with_history(
            store.state_mut(&session_a.id).unwrap(),
            "/api/input",
            "",
            &body,
        )
        .unwrap();

        assert_eq!(
            store
                .state_mut(&session_a.id)
                .unwrap()
                .model
                .get_cell_content(0, 20, 1)
                .unwrap(),
            secret
        );
        assert_eq!(
            store
                .state_mut(&session_b.id)
                .unwrap()
                .model
                .get_cell_content(0, 20, 1)
                .unwrap(),
            ""
        );
        assert_eq!(store.state_mut(&session_a.id).unwrap().app_undo.len(), 1);
        assert!(store.state_mut(&session_b.id).unwrap().app_undo.is_empty());

        let session_c = store.resolve(None).unwrap();
        let fresh = store.state_mut(&session_c.id).unwrap();
        assert_eq!(
            fresh.model.get_model().workbook.get_worksheet_names(),
            vec!["欢迎使用".to_string(), "工作表1".to_string()]
        );
        assert_eq!(fresh.model.get_cell_content(0, 1, 1).unwrap(), "UniCell");
        assert_eq!(fresh.model.get_cell_content(0, 20, 1).unwrap(), "");
        assert!(fresh.app_undo.is_empty());
        assert_eq!(fresh.model.undo_depth(), 0);
        assert!(fresh.source_ooxml.is_none());
        assert!(fresh.objects.is_empty());
    }

    #[test]
    fn session_cookie_parser_and_private_response_headers_are_strict() {
        let id = "ab".repeat(SESSION_ID_BYTES);
        assert_eq!(
            parse_session_cookie_header(&format!("theme=dark; {SESSION_COOKIE_NAME}={id}")),
            Some(id.clone())
        );
        assert!(parse_session_cookie_header("unicell_session=short").is_none());
        assert!(
            parse_session_cookie_header(&format!(
                "{SESSION_COOKIE_NAME}={}",
                "AB".repeat(SESSION_ID_BYTES)
            ))
            .is_none()
        );
        assert!(parse_session_cookie_header("other=value").is_none());
        assert!(
            parse_session_cookie_header(&format!(
                "{SESSION_COOKIE_NAME}={id}; {SESSION_COOKIE_NAME}={id}"
            ))
            .is_none()
        );

        let response = finalize_response(json_bytes_response(b"{}".to_vec(), 200), true, Some(&id));
        let header = |name: &'static str| {
            response
                .headers()
                .iter()
                .find(|header| header.field.equiv(name))
                .map(|header| header.value.as_str().to_string())
        };
        assert_eq!(
            header("Cache-Control").as_deref(),
            Some("private, no-store")
        );
        assert_eq!(header("Vary").as_deref(), Some("Cookie"));
        assert_eq!(header("Referrer-Policy").as_deref(), Some("no-referrer"));
        let cookie = header("Set-Cookie").unwrap();
        assert!(cookie.contains(&format!("{SESSION_COOKIE_NAME}={id}")));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
    }

    #[test]
    fn session_capacity_never_evicts_an_active_unsaved_workbook() {
        let mut store = SessionStore::default();
        let first = store.resolve(None).unwrap().id;
        let edit =
            serde_json::to_vec(&json!({"sheet":0,"row":20,"col":1,"value":"UNSAVED"})).unwrap();
        handle_api_with_history(store.state_mut(&first).unwrap(), "/api/input", "", &edit).unwrap();
        while store.sessions.len() < MAX_ACTIVE_SESSIONS {
            let id = store.resolve(None).unwrap().id;
            handle_api_with_history(store.state_mut(&id).unwrap(), "/api/input", "", &edit)
                .unwrap();
        }
        assert!(store.resolve(None).is_err());
        assert_eq!(
            store
                .state_mut(&first)
                .unwrap()
                .model
                .get_cell_content(0, 20, 1)
                .unwrap(),
            "UNSAVED"
        );

        store.sessions.get_mut(&first).unwrap().last_seen = std::time::Instant::now()
            .checked_sub(SESSION_IDLE_TTL + std::time::Duration::from_secs(1))
            .unwrap();
        let replacement = store.resolve(None).unwrap();
        assert!(!store.sessions.contains_key(&first));
        assert_ne!(replacement.id, first);
    }

    #[test]
    fn clean_anonymous_landings_cannot_permanently_exhaust_session_capacity() {
        let mut store = SessionStore::default();
        while store.sessions.len() < MAX_ACTIVE_SESSIONS {
            store.resolve(None).unwrap();
        }
        let before = store
            .sessions
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let replacement = store.resolve(None).unwrap();
        assert_eq!(store.sessions.len(), MAX_ACTIVE_SESSIONS);
        assert!(!before.contains(&replacement.id));
        assert!(before.iter().any(|id| !store.sessions.contains_key(id)));
    }

    fn workbook_xml(bytes: &[u8]) -> String {
        let package = snapshot_opc_package(bytes).unwrap();
        String::from_utf8(package.parts["xl/workbook.xml"].clone()).unwrap()
    }

    #[test]
    fn calculation_settings_api_is_atomic_and_unified_with_history() {
        let mut state = AppState::new();
        let get = serde_json::to_vec(&json!({"op":"get"})).unwrap();
        let initial =
            response_value(handle_api_with_history(&mut state, "/api/calcmode", "", &get).unwrap());
        assert_eq!(initial["mode"], "auto");
        assert_eq!(initial["enabled"], false);
        assert_eq!(initial["maxIterations"], 100);
        assert_eq!(state.app_undo.len(), 0);

        let set = serde_json::to_vec(&json!({
            "op":"set", "mode":"manual", "enabled":true,
            "maxIterations":250, "maxChange":0.00001
        }))
        .unwrap();
        let changed =
            response_value(handle_api_with_history(&mut state, "/api/calcmode", "", &set).unwrap());
        assert_eq!(changed["changed"], true);
        assert_eq!(state.calculation_mode, CalculationMode::Manual);
        assert_eq!(state.model.get_iteration_settings().maximum_iterations, 250);
        assert!(state.calculation_properties_dirty);
        assert_eq!(state.app_undo.len(), 1);
        assert!(state.undo_application().unwrap());
        assert_eq!(state.calculation_mode, CalculationMode::Automatic);
        assert_eq!(
            state.model.get_iteration_settings(),
            IterationSettings::default()
        );
        assert!(!state.calculation_properties_dirty);
        assert!(state.redo_application().unwrap());
        assert_eq!(state.calculation_mode, CalculationMode::Manual);
        assert_eq!(state.model.get_iteration_settings().maximum_iterations, 250);

        let invalid = serde_json::to_vec(&json!({"op":"set","maxIterations":0})).unwrap();
        assert!(handle_api_with_history(&mut state, "/api/calcmode", "", &invalid).is_err());
        assert_eq!(state.model.get_iteration_settings().maximum_iterations, 250);
    }

    #[test]
    fn calculation_properties_import_and_differential_ooxml_export_roundtrip() {
        let source_state = AppState::new();
        let base = model_to_xlsx_bytes(&source_state).unwrap();
        let mut parts = snapshot_opc_package(&base).unwrap().parts;
        let mut xml = String::from_utf8(parts["xl/workbook.xml"].clone()).unwrap();
        patch_calculation_properties_xml(
            &mut xml,
            CalculationMode::Manual,
            &IterationSettings {
                enabled: true,
                maximum_iterations: 77,
                maximum_change: 0.125,
            },
        )
        .unwrap();
        let document = roxmltree::Document::parse(&xml).unwrap();
        let start = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "calcPr")
            .unwrap()
            .range()
            .start;
        drop(document);
        let end = start + xml_start_tag_end(&xml[start..]).unwrap();
        let tagged = set_start_tag_attribute(&xml[start..=end], "futureVendor", "keep");
        xml.replace_range(start..=end, &tagged);
        parts.insert("xl/workbook.xml".to_string(), xml.into_bytes());
        let imported_bytes = encode_opc_parts(parts).unwrap();

        let mut state = AppState::new();
        load_xlsx_into_state(&mut state, &imported_bytes).unwrap();
        assert_eq!(state.calculation_mode, CalculationMode::Manual);
        assert_eq!(
            state.model.get_iteration_settings(),
            IterationSettings {
                enabled: true,
                maximum_iterations: 77,
                maximum_change: 0.125,
            }
        );
        assert!(!state.calculation_properties_dirty);
        let untouched = workbook_xml(&model_to_preserved_xlsx_bytes(&state).unwrap());
        assert!(untouched.contains("futureVendor=\"keep\""));
        assert!(untouched.contains("calcMode=\"manual\""));

        let set = serde_json::to_vec(&json!({
            "op":"set", "mode":"auto", "enabled":false,
            "maxIterations":25, "maxChange":0.25
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/calcmode", "", &set).unwrap();
        let exported = workbook_xml(&model_to_preserved_xlsx_bytes(&state).unwrap());
        assert!(exported.contains("futureVendor=\"keep\""));
        assert!(exported.contains("calcMode=\"auto\""));
        assert!(exported.contains("iterate=\"0\""));
        assert!(exported.contains("iterateCount=\"25\""));
        assert!(exported.contains("iterateDelta=\"0.25\""));
    }

    #[test]
    fn unified_history_replays_literal_rich_text_and_run_styles_atomically() {
        let mut state = AppState::new();
        let first = serde_json::to_vec(&json!({
            "sheet": 0, "row": 1, "col": 1, "content": "=001红",
            "runs": [
                {"text":"=001", "bold":true, "color":"#FF0000"},
                {"text":"红", "italic":true, "color":[4, 0.25]}
            ]
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/rich-text", "", &first).unwrap();
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "=001红");
        assert_eq!(state.rich_text[&(0, 1, 1)].len(), 2);
        assert_eq!(state.app_undo.len(), 1);

        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "");
        assert!(!state.rich_text.contains_key(&(0, 1, 1)));

        assert!(state.redo_application().unwrap());
        // Redo must use the literal-rich setter. Replaying through normal input
        // would parse the leading '=' as a formula and destroy the rich value.
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "=001红");
        assert!(state.rich_text[&(0, 1, 1)][0].bold);
        assert!(state.rich_text[&(0, 1, 1)][1].italic);

        let second = serde_json::to_vec(&json!({
            "sheet": 0, "row": 1, "col": 1, "content": "=001红!",
            "runs": [
                {"text":"=001", "bold":true, "color":"#FF0000"},
                {"text":"红!", "italic":true, "color":[4, 0.25]}
            ]
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/rich-text", "", &second).unwrap();
        assert_eq!(state.app_undo.len(), 2);
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "=001红!");
        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "=001红");
        assert!(state.redo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "=001红!");
    }

    #[test]
    fn unified_history_orders_native_objects_with_cell_edits() {
        let mut state = AppState::new();
        let add = serde_json::to_vec(&json!({
            "op":"add", "sheet":0,
            "object":{"id":"shape-1", "type":"textbox", "text":"one"}
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/objects", "", &add).unwrap();
        let input = serde_json::to_vec(&json!({
            "sheet":0, "row":2, "col":2, "value":"after object"
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/input", "", &input).unwrap();
        assert_eq!(state.app_undo.len(), 2);

        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 2, 2).unwrap(), "");
        assert_eq!(state.objects[&0].len(), 1);
        assert!(state.undo_application().unwrap());
        assert!(state.objects.get(&0).is_none_or(Vec::is_empty));

        assert!(state.redo_application().unwrap());
        assert_eq!(state.objects[&0][0]["id"], "shape-1");
        assert!(state.redo_application().unwrap());
        assert_eq!(
            state.model.get_cell_content(0, 2, 2).unwrap(),
            "after object"
        );
    }

    #[test]
    fn rich_clipboard_round_trip_preserves_excel_html_styles_merges_validation_and_objects() {
        let mut source = AppState::new();
        source.model.set_user_input(0, 1, 1, "=C1").unwrap();
        source.model.set_user_input(0, 1, 3, "7").unwrap();
        source.model.set_user_input(0, 2, 1, "merged").unwrap();
        source.model.merge_cells_range(0, 2, 1, 2, 2).unwrap();
        source
            .model
            .update_range_style(&area(0, 1, 1, 1, 1), "font.b", "true")
            .unwrap();
        api_rich_text(
            &mut source,
            "",
            &serde_json::to_vec(&json!({
                "sheet":0,"row":1,"col":2,"content":"红black",
                "runs":[
                    {"text":"红","bold":true,"color":"#FF0000"},
                    {"text":"black","italic":true,"color":"#000000"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        api_data_validation(
            &mut source,
            &serde_json::to_vec(&json!({
                "op":"add","sheet":0,
                "rule":{"sqref":"B1","type":"custom","formula1":"LEN(B1)>1","showErrorMessage":true}
            }))
            .unwrap(),
        )
        .unwrap();
        source.objects.insert(
            0,
            vec![json!({
                "id":"source-object","type":"textbox","sheet":0,"r":1,"c":1,
                "x":0,"y":0,"w":100,"h":30,"text":"copied object"
            })],
        );
        source.model.set_selected_sheet(0).unwrap();
        source.model.set_selected_cell(1, 1).unwrap();
        source.model.set_selected_range(1, 1, 2, 3).unwrap();
        let clip = serde_json::to_value(source.model.copy_to_clipboard().unwrap()).unwrap();
        let mut display = String::new();
        let mut raw = String::new();
        for row in 1..=2 {
            if row > 1 {
                display.push('\n');
                raw.push('\n');
            }
            for col in 1..=3 {
                if col > 1 {
                    display.push('\t');
                    raw.push('\t');
                }
                display.push_str(&source.model.get_formatted_cell_value(0, row, col).unwrap());
                raw.push_str(&source.model.get_cell_content(0, row, col).unwrap());
            }
        }
        let html = clipboard_html(&source, 0, 1, 1, 2, 3).unwrap();
        assert!(html.contains("ProgId"));
        assert!(html.contains("x:fmla=\"=C1\""));
        assert!(html.contains("font-weight:bold"));
        assert!(html.contains("rowspan=\"1\"") || html.contains("colspan=\"2\""));
        assert!(html.contains("color:#FF0000"));
        let payload = clipboard_payload(&source, clip, 0, 1, 1, 2, 3, &display, &raw).unwrap();

        let mut target = AppState::new();
        let request = serde_json::to_vec(&json!({
            "sheet":0,"row":5,"col":1,"text":display,
            "special":"all","unicell":payload
        }))
        .unwrap();
        handle_api_with_history(&mut target, "/api/paste", "", &request).unwrap();
        assert_eq!(target.model.get_cell_content(0, 5, 1).unwrap(), "=C5");
        assert_eq!(target.model.get_cell_content(0, 5, 2).unwrap(), "红black");
        assert_eq!(target.rich_text[&(0, 5, 2)].len(), 2);
        assert!(target.model.get_cell_style(0, 5, 1).unwrap().font.b);
        assert!(merged_ranges(&target, 0).unwrap().contains(&(6, 1, 6, 2)));
        let pasted_rule = target
            .worksheet_features
            .data_validations
            .get(&0)
            .unwrap()
            .rules
            .last()
            .unwrap();
        assert_eq!(pasted_rule.sqref, "B5");
        assert_eq!(pasted_rule.formula1.as_deref(), Some("LEN(B5)>1"));
        assert_eq!(target.objects[&0].len(), 1);
        assert_ne!(target.objects[&0][0]["id"], "source-object");
        assert_eq!(target.app_undo.len(), 1);

        assert!(target.undo_application().unwrap());
        assert_eq!(target.model.get_cell_content(0, 5, 1).unwrap(), "");
        assert!(!target.rich_text.contains_key(&(0, 5, 2)));
        assert!(target.objects.get(&0).is_none_or(Vec::is_empty));
        assert!(
            target
                .worksheet_features
                .data_validations
                .get(&0)
                .is_none_or(|sheet| sheet.rules.is_empty())
        );
        assert!(target.redo_application().unwrap());
        assert_eq!(target.model.get_cell_content(0, 5, 2).unwrap(), "红black");
        assert!(merged_ranges(&target, 0).unwrap().contains(&(6, 1, 6, 2)));
    }

    #[test]
    fn cross_sheet_cut_is_one_transaction_for_cells_refs_merge_rich_validation_cf_and_objects() {
        use ironcalc::base::{
            cf_types::{CfRule, CfRuleInput},
            types::Dxf,
        };

        let mut state = AppState::new();
        state.model.new_sheet().unwrap();
        state.model.set_user_input(0, 1, 1, "Rich").unwrap();
        state.model.set_user_input(0, 2, 1, "7").unwrap();
        state.model.set_user_input(0, 2, 2, "=A2*2").unwrap();
        state.model.set_user_input(0, 1, 4, "=A2+1").unwrap();
        state.model.set_user_input(1, 1, 1, "=Sheet1!A2+2").unwrap();
        state.model.set_user_input(1, 3, 3, "old-target").unwrap();
        state.model.set_user_input(1, 4, 4, "old-target-2").unwrap();
        state
            .model
            .update_range_style(&area(0, 2, 1, 2, 1), "font.b", "true")
            .unwrap();
        api_rich_text(
            &mut state,
            "",
            &serde_json::to_vec(&json!({
                "sheet":0,"row":1,"col":1,"content":"Rich",
                "runs":[
                    {"text":"Ri","bold":true,"color":"#FF0000"},
                    {"text":"ch","italic":true,"color":"#0000FF"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        state.model.merge_cells_range(0, 1, 1, 1, 2).unwrap();
        api_data_validation(
            &mut state,
            &serde_json::to_vec(&json!({
                "op":"add","sheet":0,
                "rule":{"sqref":"A2:B2","type":"custom","formula1":"$A$2>0"}
            }))
            .unwrap(),
        )
        .unwrap();
        api_data_validation(
            &mut state,
            &serde_json::to_vec(&json!({
                "op":"add","sheet":1,
                "rule":{"sqref":"C3:D4","type":"whole","operator":"greaterThan","formula1":"0"}
            }))
            .unwrap(),
        )
        .unwrap();
        state
            .model
            .add_conditional_formatting(
                0,
                "A2:B2",
                CfRuleInput::Formula {
                    formula: "=$A$2>0".to_string(),
                    format: Dxf::default(),
                    stop_if_true: false,
                },
            )
            .unwrap();
        state.cf_sheets.insert(0);
        state.objects.insert(
            0,
            vec![json!({
                "id":"cell-object","type":"textbox","mode":"cell","sheet":0,
                "r":2,"c":1,"x":4.0,"y":23.0,"w":80,"h":24,
                "config":{"text":"moves with cells"}
            })],
        );
        state
            .model
            .new_defined_name("MovedGlobal", None, "Sheet1!$A$2")
            .unwrap();
        state
            .model
            .new_defined_name("MovedLocal", Some(0), "Sheet1!$A$2")
            .unwrap();

        state.model.set_selected_sheet(0).unwrap();
        state.model.set_selected_cell(1, 1).unwrap();
        state.model.set_selected_range(1, 1, 2, 2).unwrap();
        let clipboard = state.model.copy_to_clipboard().unwrap();
        let clip_json = serde_json::to_value(&clipboard).unwrap();
        let display = (1..=2)
            .map(|row| {
                (1..=2)
                    .map(|column| {
                        state
                            .model
                            .get_formatted_cell_value(0, row, column)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n");
        state.clip_tsv = Some(display.clone());
        state.clip_engine = Some(clip_json);
        state.clear_application_history();
        let (source_object_x, source_object_y) = cell_origin_pixels(&state.model, 0, 2, 1);
        let (target_object_x, target_object_y) = cell_origin_pixels(&state.model, 1, 4, 3);
        let expected_object_x = target_object_x + 4.0 - source_object_x;
        let expected_object_y = target_object_y + 23.0 - source_object_y;

        handle_api_with_history(
            &mut state,
            "/api/paste",
            "",
            &serde_json::to_vec(&json!({
                "sheet":1,"row":3,"col":3,"text":display,"mode":"cut","special":"all"
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(state.app_undo.len(), 1);
        assert!(state.clip_tsv.is_none());
        assert!(state.clip_engine.is_none());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "");
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "");
        assert_eq!(state.model.get_cell_content(1, 3, 3).unwrap(), "Rich");
        assert_eq!(state.model.get_cell_content(1, 4, 3).unwrap(), "7");
        assert_eq!(state.model.get_cell_content(1, 4, 4).unwrap(), "=C4*2");
        assert!(
            state
                .model
                .get_cell_content(0, 1, 4)
                .unwrap()
                .contains("Sheet2!C4")
        );
        assert!(
            state
                .model
                .get_cell_content(1, 1, 1)
                .unwrap()
                .contains("C4")
        );
        assert!(state.model.get_cell_style(1, 4, 3).unwrap().font.b);
        assert!(merged_ranges(&state, 0).unwrap().is_empty());
        assert!(merged_ranges(&state, 1).unwrap().contains(&(3, 3, 3, 4)));
        assert!(!state.rich_text.contains_key(&(0, 1, 1)));
        assert_eq!(state.rich_text[&(1, 3, 3)].len(), 2);
        assert!(!state.rich_text_xml.contains_key(&(0, 1, 1)));
        assert!(state.rich_text_xml.contains_key(&(1, 3, 3)));
        let source_validations = &state.worksheet_features.data_validations[&0].rules;
        assert!(source_validations.is_empty());
        let target_validations = &state.worksheet_features.data_validations[&1].rules;
        assert_eq!(target_validations.len(), 1);
        assert_eq!(target_validations[0].sqref, "C4:D4");
        assert_eq!(target_validations[0].formula1.as_deref(), Some("$C$4>0"));
        assert!(
            state
                .model
                .get_conditional_formatting_list(0)
                .unwrap()
                .is_empty()
        );
        let target_cf = state.model.get_conditional_formatting_list(1).unwrap();
        assert_eq!(target_cf[0].range, "C4:D4");
        match &target_cf[0].cf_rule {
            CfRule::Formula { formula, .. } => assert_eq!(formula, "=$C$4>0"),
            other => panic!("expected formula CF, got {other:?}"),
        }
        assert!(state.objects.get(&0).is_none_or(Vec::is_empty));
        let object = &state.objects[&1][0];
        assert_eq!(object["r"], 4);
        assert_eq!(object["c"], 3);
        assert_eq!(object["x"], expected_object_x);
        assert_eq!(object["y"], expected_object_y);
        assert!(state.model.get_defined_name_list().contains(&(
            "MovedLocal".to_string(),
            Some(0),
            "Sheet2!$C$4".to_string()
        )));

        assert!(state.undo_application().unwrap());
        assert_eq!(state.clip_tsv.as_deref(), Some(display.as_str()));
        assert!(state.clip_engine.is_some());
        let undo_selection = state.model.get_selected_view();
        assert_eq!(undo_selection.sheet, 0);
        assert_eq!(undo_selection.range, [1, 1, 2, 2]);
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "Rich");
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "7");
        assert_eq!(state.model.get_cell_content(1, 3, 3).unwrap(), "old-target");
        assert_eq!(
            state.model.get_cell_content(1, 4, 4).unwrap(),
            "old-target-2"
        );
        assert!(merged_ranges(&state, 0).unwrap().contains(&(1, 1, 1, 2)));
        assert!(merged_ranges(&state, 1).unwrap().is_empty());
        assert_eq!(state.rich_text[&(0, 1, 1)].len(), 2);
        assert!(!state.rich_text.contains_key(&(1, 3, 3)));
        assert!(state.rich_text_xml.contains_key(&(0, 1, 1)));
        assert!(!state.rich_text_xml.contains_key(&(1, 3, 3)));
        assert_eq!(state.worksheet_features.data_validations[&0].rules.len(), 1);
        assert_eq!(state.worksheet_features.data_validations[&1].rules.len(), 1);
        assert_eq!(state.objects[&0][0]["id"], "cell-object");
        assert!(state.objects.get(&1).is_none_or(Vec::is_empty));
        assert_eq!(
            state.model.get_conditional_formatting_list(0).unwrap()[0].range,
            "A2:B2"
        );
        assert!(
            state
                .model
                .get_conditional_formatting_list(1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(state.model.get_cell_content(0, 1, 4).unwrap(), "=A2+1");
        assert_eq!(
            state.model.get_cell_content(1, 1, 1).unwrap(),
            "=Sheet1!A2+2"
        );

        assert!(state.redo_application().unwrap());
        assert!(state.clip_tsv.is_none());
        assert!(state.clip_engine.is_none());
        let redo_selection = state.model.get_selected_view();
        assert_eq!(redo_selection.sheet, 1);
        assert_eq!(redo_selection.range, [3, 3, 4, 4]);
        assert_eq!(state.model.get_cell_content(1, 4, 4).unwrap(), "=C4*2");
        assert!(merged_ranges(&state, 1).unwrap().contains(&(3, 3, 3, 4)));
        assert_eq!(state.rich_text[&(1, 3, 3)].len(), 2);
        assert!(state.rich_text_xml.contains_key(&(1, 3, 3)));
        assert_eq!(state.objects[&1][0]["id"], "cell-object");
    }

    #[test]
    fn cross_sheet_cut_rejects_partial_target_merge_and_native_drawing_before_writes() {
        let mut state = AppState::new();
        state.model.new_sheet().unwrap();
        state.model.set_user_input(0, 1, 1, "source").unwrap();
        state.model.set_user_input(1, 2, 2, "target").unwrap();
        state.model.merge_cells_range(1, 2, 2, 2, 3).unwrap();
        state.model.set_selected_sheet(0).unwrap();
        state.model.set_selected_cell(1, 1).unwrap();
        state.model.set_selected_range(1, 1, 1, 1).unwrap();
        let clipboard = state.model.copy_to_clipboard().unwrap();
        state.clip_tsv = Some("source".to_string());
        state.clip_engine = Some(serde_json::to_value(clipboard).unwrap());
        let request = serde_json::to_vec(&json!({
            "sheet":1,"row":2,"col":2,"text":"source","mode":"cut","special":"all"
        }))
        .unwrap();
        assert!(handle_api_with_history(&mut state, "/api/paste", "", &request).is_err());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "source");
        assert_eq!(state.model.get_cell_content(1, 2, 2).unwrap(), "target");
        assert!(merged_ranges(&state, 1).unwrap().contains(&(2, 2, 2, 3)));
        assert_eq!(state.clip_tsv.as_deref(), Some("source"));
        assert!(state.clip_engine.is_some());
        assert!(state.app_undo.is_empty());

        state.model.unmerge_cells_range(1, 2, 2, 2, 3).unwrap();
        state.objects.insert(
            0,
            vec![json!({
                "id":"native","sheet":0,"mode":"abs","r":1,"c":1,
                "x":0.0,"y":0.0,"w":150.0,"h":40.0,
                "config":{"nativeDrawing":{"anchorKind":"twoCellAnchor","kind":"chart"}}
            })],
        );
        let native_error = match handle_api_with_history(&mut state, "/api/paste", "", &request) {
            Err(error) => error,
            Ok(_) => panic!("partial two-cell anchor cut unexpectedly succeeded"),
        };
        assert!(native_error.contains("part of cell-anchored object"));
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "source");
        assert_eq!(state.model.get_cell_content(1, 2, 2).unwrap(), "target");
        assert_eq!(state.objects[&0][0]["id"], "native");
        assert!(state.objects.get(&1).is_none_or(Vec::is_empty));
        assert_eq!(state.clip_tsv.as_deref(), Some("source"));
        assert!(state.clip_engine.is_some());
        assert!(state.app_undo.is_empty());
    }

    #[test]
    fn same_sheet_cut_rejects_excel_table_intersection_without_consuming_clipboard() {
        let mut state = AppState::new();
        state.model.set_user_input(0, 1, 1, "Header").unwrap();
        state.model.set_user_input(0, 2, 1, "source").unwrap();
        state.model.set_user_input(0, 4, 4, "target").unwrap();
        state
            .model
            .replace_tables(std::collections::HashMap::from([(
                "Table1".to_string(),
                Table {
                    name: "Table1".to_string(),
                    display_name: "Table1".to_string(),
                    sheet_name: "Sheet1".to_string(),
                    reference: "A1:B2".to_string(),
                    totals_row_count: 0,
                    header_row_count: 1,
                    header_row_dxf_id: None,
                    data_dxf_id: None,
                    totals_row_dxf_id: None,
                    columns: vec![
                        TableColumn {
                            id: 1,
                            name: "Header".to_string(),
                            ..Default::default()
                        },
                        TableColumn {
                            id: 2,
                            name: "Value".to_string(),
                            ..Default::default()
                        },
                    ],
                    style_info: TableStyleInfo::default(),
                    has_filters: true,
                },
            )]));
        state.model.set_selected_sheet(0).unwrap();
        state.model.set_selected_cell(2, 1).unwrap();
        state.model.set_selected_range(2, 1, 2, 1).unwrap();
        let clipboard = serde_json::to_value(state.model.copy_to_clipboard().unwrap()).unwrap();
        state.clip_tsv = Some("source".to_string());
        state.clip_engine = Some(clipboard.clone());
        state.clear_application_history();
        let model_depth = state.model.undo_depth();
        let request = serde_json::to_vec(&json!({
            "sheet":0,"row":4,"col":4,"text":"source","mode":"cut","special":"all"
        }))
        .unwrap();

        let error = match handle_api_with_history(&mut state, "/api/paste", "", &request) {
            Err(error) => error,
            Ok(_) => panic!("cut through an Excel table unexpectedly succeeded"),
        };
        assert!(error.contains("Excel table 'Table1'"));
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "source");
        assert_eq!(state.model.get_cell_content(0, 4, 4).unwrap(), "target");
        assert_eq!(state.clip_tsv.as_deref(), Some("source"));
        assert_eq!(state.clip_engine.as_ref(), Some(&clipboard));
        assert_eq!(state.model.undo_depth(), model_depth);
        assert!(state.app_undo.is_empty());
        let selection = state.model.get_selected_view();
        assert_eq!(selection.sheet, 0);
        assert_eq!(selection.range, [2, 1, 2, 1]);
    }

    #[test]
    fn cross_sheet_cut_rejects_x14_cf_and_validation_on_source_or_target_atomically() {
        fn snapshot_with_x14_rule(
            rule_sheet: usize,
            kind: &str,
            sqref: &str,
        ) -> OpcPackageSnapshot {
            let workbook = br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/><sheet name="Sheet2" sheetId="2" r:id="rId2"/></sheets></workbook>"#;
            let relationships = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#;
            let rule = match kind {
                "cf" => format!(
                    r#"<x14:conditionalFormatting><x14:cfRule type="expression" id="{{00000000-0000-0000-0000-000000000001}}"/><xm:sqref>{sqref}</xm:sqref></x14:conditionalFormatting>"#,
                ),
                "dv" => format!(
                    r#"<x14:dataValidation type="custom"><x14:formula1><xm:f>1=1</xm:f></x14:formula1><xm:sqref>{sqref}</xm:sqref></x14:dataValidation>"#,
                ),
                _ => unreachable!(),
            };
            let worksheet = |with_rule: bool| {
                format!(
                    r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><sheetData/>{}</worksheet>"#,
                    if with_rule { rule.as_str() } else { "" }
                )
                .into_bytes()
            };
            OpcPackageSnapshot {
                parts: std::collections::BTreeMap::from([
                    ("xl/workbook.xml".to_string(), workbook.to_vec()),
                    (
                        "xl/_rels/workbook.xml.rels".to_string(),
                        relationships.to_vec(),
                    ),
                    (
                        "xl/worksheets/sheet1.xml".to_string(),
                        worksheet(rule_sheet == 0),
                    ),
                    (
                        "xl/worksheets/sheet2.xml".to_string(),
                        worksheet(rule_sheet == 1),
                    ),
                ]),
                macro_enabled: false,
            }
        }

        for (kind, rule_sheet, sqref) in [
            ("cf", 0usize, "A1:A2"),
            ("cf", 1usize, "B2:C2"),
            ("dv", 0usize, "A1:A2"),
            ("dv", 1usize, "B2:C2"),
        ] {
            let mut state = AppState::new();
            state.model.new_sheet().unwrap();
            state.model.set_user_input(0, 1, 1, "source").unwrap();
            state.model.set_user_input(1, 2, 2, "target").unwrap();
            state.source_ooxml = Some(snapshot_with_x14_rule(rule_sheet, kind, sqref));
            state.model.set_selected_sheet(0).unwrap();
            state.model.set_selected_cell(1, 1).unwrap();
            state.model.set_selected_range(1, 1, 1, 1).unwrap();
            let clipboard = serde_json::to_value(state.model.copy_to_clipboard().unwrap()).unwrap();
            state.clip_tsv = Some("source".to_string());
            state.clip_engine = Some(clipboard.clone());
            state.clear_application_history();
            let model_depth = state.model.undo_depth();
            let request = serde_json::to_vec(&json!({
                "sheet":1,"row":2,"col":2,"text":"source","mode":"cut","special":"all"
            }))
            .unwrap();

            let error = match handle_api_with_history(&mut state, "/api/paste", "", &request) {
                Err(error) => error,
                Ok(_) => panic!("cross-sheet cut through x14 range unexpectedly succeeded"),
            };
            assert!(
                error.contains("preserve-only x14"),
                "{kind}/{rule_sheet}: {error}"
            );
            assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "source");
            assert_eq!(state.model.get_cell_content(1, 2, 2).unwrap(), "target");
            assert_eq!(state.clip_tsv.as_deref(), Some("source"));
            assert_eq!(state.clip_engine.as_ref(), Some(&clipboard));
            assert_eq!(state.model.undo_depth(), model_depth);
            assert!(state.app_undo.is_empty());
            let selection = state.model.get_selected_view();
            assert_eq!(selection.sheet, 0);
            assert_eq!(selection.range, [1, 1, 1, 1]);
        }
    }

    #[test]
    fn late_cut_failure_rolls_back_cells_clipboard_and_selection() {
        let mut state = AppState::new();
        state.model.new_sheet().unwrap();
        state.model.set_user_input(0, 1, 1, "source").unwrap();
        state.model.set_user_input(1, 2, 1, "old-target").unwrap();
        // The destination is directly below this native table.  Its deliberately missing table
        // part makes automatic expansion fail only after the engine move, exercising the HTTP
        // transaction rollback rather than an early preflight rejection.
        state.native_table_model = Some(json!({"tables":[{
            "sheet":"Sheet2","reference":"A1:A1","totalsRowCount":0,
            "headerRowCount":1,"part":"xl/tables/missing.xml","columns":[{"id":1,"name":"A"}]
        }]}));
        state.model.set_selected_sheet(0).unwrap();
        state.model.set_selected_cell(1, 1).unwrap();
        state.model.set_selected_range(1, 1, 1, 1).unwrap();
        let clipboard = serde_json::to_value(state.model.copy_to_clipboard().unwrap()).unwrap();
        state.clip_tsv = Some("source".to_string());
        state.clip_engine = Some(clipboard.clone());
        state.clear_application_history();
        let model_depth = state.model.undo_depth();
        let request = serde_json::to_vec(&json!({
            "sheet":1,"row":2,"col":1,"text":"source","mode":"cut","special":"all"
        }))
        .unwrap();

        assert!(handle_api_with_history(&mut state, "/api/paste", "", &request).is_err());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "source");
        assert_eq!(state.model.get_cell_content(1, 2, 1).unwrap(), "old-target");
        assert_eq!(state.clip_tsv.as_deref(), Some("source"));
        assert_eq!(state.clip_engine.as_ref(), Some(&clipboard));
        assert_eq!(state.model.undo_depth(), model_depth);
        assert!(state.app_undo.is_empty());
        let selection = state.model.get_selected_view();
        assert_eq!(selection.sheet, 0);
        assert_eq!(selection.range, [1, 1, 1, 1]);
    }

    #[test]
    fn paste_special_values_uses_unformatted_calculation_results_without_precision_loss() {
        let mut source = AppState::new();
        source.model.set_user_input(0, 1, 1, "=B1").unwrap();
        source
            .model
            .set_user_input(0, 1, 2, "7.1250000000001")
            .unwrap();
        source
            .model
            .update_range_style(&area(0, 1, 1, 1, 1), "num_fmt", "0.0")
            .unwrap();
        source.model.set_selected_sheet(0).unwrap();
        source.model.set_selected_cell(1, 1).unwrap();
        source.model.set_selected_range(1, 1, 1, 1).unwrap();
        let clip = serde_json::to_value(source.model.copy_to_clipboard().unwrap()).unwrap();
        let display = source.model.get_formatted_cell_value(0, 1, 1).unwrap();
        assert_eq!(display, "7.1");
        let payload = clipboard_payload(&source, clip, 0, 1, 1, 1, 1, &display, "=B1").unwrap();

        let mut target = AppState::new();
        handle_api_with_history(
            &mut target,
            "/api/paste",
            "",
            &serde_json::to_vec(&json!({
                "sheet":0,"row":3,"col":2,"text":display,
                "special":"values","unicell":payload
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            target.model.get_cell_content(0, 3, 2).unwrap(),
            "7.1250000000001"
        );
        assert!(matches!(
            target
                .model
                .get_model()
                .get_cell_value_by_index(0, 3, 2)
                .unwrap(),
            CellValue::Number(value) if (value - 7.1250000000001).abs() < 1e-13
        ));
        assert_eq!(target.app_undo.len(), 1);
        assert!(target.undo_application().unwrap());
        assert_eq!(target.model.get_cell_content(0, 3, 2).unwrap(), "");
    }

    #[test]
    fn native_table_api_creates_exports_and_undoes_a_real_excel_table() {
        let mut state = AppState::new();
        for (row, values) in [(1, ["Name", "Amount"]), (2, ["A", "10"]), (3, ["B", "20"])] {
            for (column, value) in values.into_iter().enumerate() {
                state
                    .model
                    .set_user_input(0, row, column as i32 + 1, value)
                    .unwrap();
            }
        }
        // These authoring writes predate the HTTP transaction under test.
        state.clear_application_history();
        let request = serde_json::to_vec(&json!({
            "op":"update",
            "patch":{"createTables":[{
                "sheetPart":"xl/worksheets/sheet1.xml",
                "name":"SalesTable",
                "ref":"A1:B3",
                "columns":["Name","Amount"],
                "styleInfo":{"name":"TableStyleMedium2","showRowStripes":true}
            }]}
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/tables", "", &request).unwrap();
        let model = table_model_with_edits(&state).unwrap();
        assert_eq!(model["tables"].as_array().unwrap().len(), 1);
        assert_eq!(model["tables"][0]["displayName"], "SalesTable");
        assert_eq!(state.app_undo.len(), 1);

        let exported = unzip_test_parts(model_to_preserved_xlsx_bytes(&state).unwrap());
        let table_part = exported
            .iter()
            .find(|(name, _)| name.starts_with("xl/tables/") && name.ends_with(".xml"))
            .map(|(_, bytes)| String::from_utf8(bytes.clone()).unwrap())
            .expect("native table part");
        assert!(table_part.contains("displayName=\"SalesTable\""));
        assert!(table_part.contains("ref=\"A1:B3\""));
        let sheet = String::from_utf8(exported["xl/worksheets/sheet1.xml"].clone()).unwrap();
        assert!(sheet.contains("<tableParts"));

        assert!(state.undo_application().unwrap());
        assert!(
            table_model_with_edits(&state).unwrap()["tables"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(state.redo_application().unwrap());
        assert_eq!(
            table_model_with_edits(&state).unwrap()["tables"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn table_definition_runtime_sort_filter_and_native_journal_undo_together() {
        let mut state = AppState::new();
        for (row, values) in [
            (1, ["Name", "Amount"]),
            (2, ["b", "20"]),
            (3, ["a", "10"]),
            (4, ["c", "30"]),
        ] {
            for (column, value) in values.into_iter().enumerate() {
                state
                    .model
                    .set_user_input(0, row, column as i32 + 1, value)
                    .unwrap();
            }
        }
        state.clear_application_history();
        let request = serde_json::to_vec(&json!({
            "op":"update",
            "patch":{"createTables":[{
                "sheetPart":"xl/worksheets/sheet1.xml","name":"RuntimeTable","ref":"A1:B4",
                "columns":["Name","Amount"]
            }]},
            "runtime":[
                {"type":"sort","request":{"sheet":0,"r0":2,"c0":1,"r1":4,"c1":2,"headerRows":0,
                    "conditions":[{"col":1,"order":"ascending"}]}},
                {"type":"filter","request":{"sheet":0,"r0":1,"c0":1,"r1":4,"c1":2,"headerRows":1,
                    "filters":[{"col":1,"kind":"values","values":["a"]}]}}
            ]
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/tables", "", &request).unwrap();
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "a");
        assert!(!state.model.get_model().is_row_hidden(0, 2).unwrap());
        assert!(state.model.get_model().is_row_hidden(0, 3).unwrap());
        assert_eq!(
            table_model_with_edits(&state).unwrap()["tables"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(state.app_undo.len(), 1);

        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "b");
        for row in 2..=4 {
            assert!(!state.model.get_model().is_row_hidden(0, row).unwrap());
        }
        assert!(
            table_model_with_edits(&state).unwrap()["tables"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(state.redo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "a");
        assert!(state.model.get_model().is_row_hidden(0, 3).unwrap());
        assert_eq!(
            table_model_with_edits(&state).unwrap()["tables"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn calculated_table_columns_fill_and_adjacent_input_auto_expands_structured_references() {
        let mut state = AppState::new();
        for (row, values) in [
            (1, ["Item", "Amount", "Double"]),
            (2, ["a", "10", ""]),
            (3, ["b", "20", ""]),
        ] {
            for (column, value) in values.into_iter().enumerate() {
                state
                    .model
                    .set_user_input(0, row, column as i32 + 1, value)
                    .unwrap();
            }
        }
        state.clear_application_history();
        handle_api_with_history(
            &mut state,
            "/api/tables",
            "",
            &serde_json::to_vec(&json!({
                "op":"update","patch":{"createTables":[{
                    "sheetPart":"xl/worksheets/sheet1.xml","name":"CalcTable","ref":"A1:C3",
                    "columns":[
                        {"name":"Item"},{"name":"Amount"},
                        {"name":"Double","calculatedColumnFormula":"=[@Amount]*2"}
                    ]
                }]}
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(state.model.get_cell_content(0, 2, 3).unwrap(), "=$B$2*2");
        assert_eq!(state.model.get_formatted_cell_value(0, 2, 3).unwrap(), "20");
        assert_eq!(state.model.get_formatted_cell_value(0, 3, 3).unwrap(), "40");

        handle_api_with_history(
            &mut state,
            "/api/input",
            "",
            &serde_json::to_vec(&json!({"sheet":0,"row":4,"col":1,"value":"c"})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            state.native_table_model.as_ref().unwrap()["tables"][0]["reference"],
            "A1:C4"
        );
        assert_eq!(
            state.model.get_model().workbook.tables["CalcTable"].reference,
            "A1:C4"
        );
        assert_eq!(state.model.get_cell_content(0, 4, 3).unwrap(), "=$B$4*2");
        handle_api_with_history(
            &mut state,
            "/api/input",
            "",
            &serde_json::to_vec(&json!({"sheet":0,"row":4,"col":2,"value":"30"})).unwrap(),
        )
        .unwrap();
        assert_eq!(state.model.get_formatted_cell_value(0, 4, 3).unwrap(), "60");

        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 4, 2).unwrap(), "");
        assert!(state.undo_application().unwrap());
        assert_eq!(
            state.native_table_model.as_ref().unwrap()["tables"][0]["reference"],
            "A1:C3"
        );
        assert_eq!(state.model.get_cell_content(0, 4, 1).unwrap(), "");
        assert_eq!(state.model.get_cell_content(0, 4, 3).unwrap(), "");
        assert!(state.redo_application().unwrap());
        assert_eq!(
            state.native_table_model.as_ref().unwrap()["tables"][0]["reference"],
            "A1:C4"
        );
    }

    #[test]
    fn batch_and_external_paste_expand_tables_without_overwriting_manual_calculated_exceptions() {
        let mut state = AppState::new();
        for (row, values) in [
            (1, ["Item", "Amount", "Double"]),
            (2, ["a", "10", ""]),
            (3, ["b", "20", ""]),
        ] {
            for (column, value) in values.into_iter().enumerate() {
                state
                    .model
                    .set_user_input(0, row, column as i32 + 1, value)
                    .unwrap();
            }
        }
        handle_api_with_history(
            &mut state,
            "/api/tables",
            "",
            &serde_json::to_vec(&json!({
                "op":"update","patch":{"createTables":[{
                    "sheetPart":"xl/worksheets/sheet1.xml","name":"PasteTable","ref":"A1:C3",
                    "columns":[
                        {"name":"Item"},{"name":"Amount"},
                        {"name":"Double","calculatedColumnFormula":"=[@Amount]*2"}
                    ]
                }]}
            }))
            .unwrap(),
        )
        .unwrap();
        state.clear_application_history();

        // Later row first: expansion must still be ordered and keep the explicit 777 exception.
        handle_api_with_history(
            &mut state,
            "/api/batch",
            "",
            &serde_json::to_vec(&json!({"sheet":0,"cells":[
                {"r":5,"c":1,"v":"e"},{"r":5,"c":2,"v":"30"},
                {"r":4,"c":1,"v":"d"},{"r":4,"c":2,"v":"25"},{"r":4,"c":3,"v":"777"}
            ]}))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            state.native_table_model.as_ref().unwrap()["tables"][0]["reference"],
            "A1:C5"
        );
        assert_eq!(state.model.get_cell_content(0, 4, 3).unwrap(), "777");
        assert_eq!(state.model.get_formatted_cell_value(0, 5, 3).unwrap(), "60");

        handle_api_with_history(
            &mut state,
            "/api/paste",
            "",
            &serde_json::to_vec(&json!({
                "sheet":0,"row":6,"col":1,"mode":"copy","text":"f\t40\t901\ng\t50\t"
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            state.native_table_model.as_ref().unwrap()["tables"][0]["reference"],
            "A1:C7"
        );
        assert_eq!(state.model.get_cell_content(0, 4, 3).unwrap(), "777");
        assert_eq!(state.model.get_cell_content(0, 6, 3).unwrap(), "901");
        assert_eq!(
            state.model.get_formatted_cell_value(0, 7, 3).unwrap(),
            "100"
        );
        handle_api_with_history(
            &mut state,
            "/api/input",
            "",
            &serde_json::to_vec(&json!({"sheet":0,"row":2,"col":4,"value":"tag-a"})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            state.native_table_model.as_ref().unwrap()["tables"][0]["reference"],
            "A1:D7"
        );
        assert_eq!(
            state.native_table_model.as_ref().unwrap()["tables"][0]["columns"][3]["name"],
            "Column4"
        );
        assert_eq!(state.model.get_cell_content(0, 1, 4).unwrap(), "Column4");
        assert_eq!(state.model.get_cell_content(0, 2, 4).unwrap(), "tag-a");
        assert!(state.undo_application().unwrap());
        assert_eq!(
            state.native_table_model.as_ref().unwrap()["tables"][0]["reference"],
            "A1:C7"
        );
        assert!(state.undo_application().unwrap());
        assert_eq!(
            state.native_table_model.as_ref().unwrap()["tables"][0]["reference"],
            "A1:C5"
        );
    }

    #[test]
    fn runtime_sort_moves_formulas_rich_styles_and_objects_as_one_undoable_transaction() {
        let mut state = AppState::new();
        for (row, values) in [
            (1, ["Key", "Result", "Label"]),
            (2, ["b", "=C2", "row-b"]),
            (3, ["a", "=C3", "row-a"]),
            (4, ["c", "=C4", "row-c"]),
        ] {
            for (column, value) in values.into_iter().enumerate() {
                state
                    .model
                    .set_user_input(0, row, column as i32 + 1, value)
                    .unwrap();
            }
        }
        state
            .model
            .update_range_style(&area(0, 3, 1, 3, 1), "font.b", "true")
            .unwrap();
        api_rich_text(
            &mut state,
            "",
            &serde_json::to_vec(&json!({
                "sheet":0,"row":3,"col":3,"content":"row-a",
                "runs":[{"text":"row-","bold":true},{"text":"a","italic":true}]
            }))
            .unwrap(),
        )
        .unwrap();
        state.objects.insert(
            0,
            vec![json!({"id":"sort-object","type":"textbox","sheet":0,"r":3,"c":1})],
        );
        state.clear_application_history();

        handle_api_with_history(
            &mut state,
            "/api/sort",
            "",
            &serde_json::to_vec(&json!({
                "sheet":0,"r0":1,"c0":1,"r1":4,"c1":3,"headerRows":1,
                "conditions":[{"col":1,"order":"ascending"}]
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "a");
        assert_eq!(state.model.get_cell_content(0, 2, 2).unwrap(), "=C2");
        assert_eq!(
            state.model.get_formatted_cell_value(0, 2, 2).unwrap(),
            "row-a"
        );
        assert!(state.model.get_cell_style(0, 2, 1).unwrap().font.b);
        assert!(state.rich_text[&(0, 2, 3)][0].bold);
        assert_eq!(state.objects[&0][0]["r"], 2);
        assert_eq!(state.app_undo.len(), 1);

        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "b");
        assert_eq!(state.model.get_cell_content(0, 3, 1).unwrap(), "a");
        assert!(state.rich_text[&(0, 3, 3)][0].bold);
        assert_eq!(state.objects[&0][0]["r"], 3);
        assert!(state.redo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 2, 1).unwrap(), "a");
    }

    #[test]
    fn runtime_filter_supports_value_custom_top_and_clear_with_unified_undo() {
        let mut state = AppState::new();
        for (row, values) in [
            (1, ["Region", "Amount"]),
            (2, ["East", "10"]),
            (3, ["West", "40"]),
            (4, ["East", "30"]),
            (5, ["North", "20"]),
        ] {
            for (column, value) in values.into_iter().enumerate() {
                state
                    .model
                    .set_user_input(0, row, column as i32 + 1, value)
                    .unwrap();
            }
        }
        state.clear_application_history();
        handle_api_with_history(
            &mut state,
            "/api/filter",
            "",
            &serde_json::to_vec(&json!({
                "sheet":0,"r0":1,"c0":1,"r1":5,"c1":2,"headerRows":1,
                "filters":[
                    {"col":1,"kind":"values","values":["East","West"]},
                    {"col":2,"kind":"custom","operator":"greaterThanOrEqual","value":"30"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(state.model.get_model().is_row_hidden(0, 2).unwrap());
        assert!(!state.model.get_model().is_row_hidden(0, 3).unwrap());
        assert!(!state.model.get_model().is_row_hidden(0, 4).unwrap());
        assert!(state.model.get_model().is_row_hidden(0, 5).unwrap());
        assert_eq!(state.app_undo.len(), 1);
        assert!(state.undo_application().unwrap());
        for row in 2..=5 {
            assert!(!state.model.get_model().is_row_hidden(0, row).unwrap());
        }
        assert!(state.redo_application().unwrap());
        assert!(state.model.get_model().is_row_hidden(0, 2).unwrap());

        handle_api_with_history(
            &mut state,
            "/api/filter",
            "",
            &serde_json::to_vec(&json!({
                "sheet":0,"r0":1,"c0":1,"r1":5,"c1":2,"headerRows":1,
                "filters":[{"col":2,"kind":"top10","count":2,"direction":"top"}]
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(state.model.get_model().is_row_hidden(0, 2).unwrap());
        assert!(!state.model.get_model().is_row_hidden(0, 3).unwrap());
        assert!(!state.model.get_model().is_row_hidden(0, 4).unwrap());
        assert!(state.model.get_model().is_row_hidden(0, 5).unwrap());

        handle_api_with_history(
            &mut state,
            "/api/filter",
            "",
            &serde_json::to_vec(&json!({
                "sheet":0,"r0":1,"c0":1,"r1":5,"c1":2,"headerRows":1,"clear":true
            }))
            .unwrap(),
        )
        .unwrap();
        for row in 2..=5 {
            assert!(!state.model.get_model().is_row_hidden(0, row).unwrap());
        }
    }

    #[test]
    fn page_layout_review_protection_and_comments_export_and_undo_atomically() {
        let mut state = AppState::new();
        state.clear_application_history();
        let request = serde_json::to_vec(&json!({
            "op":"update",
            "patch":{
                "workbookProtection":{"lockStructure":true},
                "worksheets":[{
                    "localSheetId":0,
                    "pageMargins":{"left":0.25,"right":0.25,"top":0.5,"bottom":0.5},
                    "pageSetup":{"paperSize":"A4","orientation":"landscape","fitToWidth":1,"fitToHeight":0},
                    "printOptions":{"gridLines":true,"horizontalCentered":true},
                    "headerFooter":{"oddHeader":"&CQuarter &A","oddFooter":"Page &P of &N"},
                    "rowBreaks":[{"id":20,"min":0,"max":16383,"man":true}],
                    "sheetProtection":{"sheet":true,"selectUnlockedCells":true},
                    "protectedRanges":[{"name":"Input","sqref":"A2:B20"}],
                    "printArea":"Sheet1!$A$1:$F$20",
                    "printTitles":"Sheet1!$1:$2",
                    "notes":{"upsert":[{"ref":"B3","author":"Alice","text":"legacy note"}]},
                    "threadedComments":{"upsert":[{
                        "ref":"C4","author":"Chen","text":"threaded comment",
                        "dateTime":"2026-08-05T01:02:03Z"
                    }]}
                }]
            }
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/page-review", "", &request).unwrap();
        let model = page_review_model_with_edits(&state).unwrap();
        assert_eq!(model["worksheets"][0]["pageSetup"]["paperSize"], "9");
        assert_eq!(
            model["worksheets"][0]["pageSetup"]["orientation"],
            "landscape"
        );
        assert_eq!(
            model["worksheets"][0]["notes"]["items"][0]["text"],
            "legacy note"
        );
        assert_eq!(
            model["worksheets"][0]["threadedComments"]["items"][0]["text"],
            "threaded comment"
        );
        assert_eq!(state.app_undo.len(), 1);

        let exported = unzip_test_parts(model_to_preserved_xlsx_bytes(&state).unwrap());
        let sheet = String::from_utf8(exported["xl/worksheets/sheet1.xml"].clone()).unwrap();
        assert!(sheet.contains("paperSize=\"9\""));
        assert!(sheet.contains("orientation=\"landscape\""));
        assert!(sheet.contains("<pageMargins"));
        assert!(sheet.contains("<rowBreaks"));
        assert!(sheet.contains("<legacyDrawing"));
        let workbook = String::from_utf8(exported["xl/workbook.xml"].clone()).unwrap();
        assert!(workbook.contains("_xlnm.Print_Area"));
        assert!(workbook.contains("_xlnm.Print_Titles"));
        assert!(exported.keys().any(|part| part.starts_with("xl/comments")));
        assert!(exported.keys().any(|part| part.contains("threadedComment")));

        assert!(state.undo_application().unwrap());
        let undone = page_review_model_with_edits(&state).unwrap();
        assert!(undone["worksheets"][0]["pageSetup"].is_null());
        assert!(
            undone["worksheets"][0]["notes"].is_null()
                || undone["worksheets"][0]["notes"]["items"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
        );
        assert!(state.redo_application().unwrap());
        assert_eq!(
            page_review_model_with_edits(&state).unwrap()["worksheets"][0]["pageSetup"]["paperSize"],
            "9"
        );
    }

    #[test]
    fn protection_passwords_are_hashed_before_the_native_journal_and_verified_on_change() {
        let mut state = AppState::new();
        let request = serde_json::to_vec(&json!({
            "op":"update",
            "patch":{"worksheets":[{
                "sheet":"Sheet1",
                "sheetProtection":{"sheet":true},
                "protectedRanges":{"upsert":[{"name":"Input","sqref":"A2:B3"}]}
            }]},
            "passwordChanges":{
                "worksheets":[{"sheet":"Sheet1","password":"sheet-secret"}],
                "protectedRanges":[{"sheet":"Sheet1","name":"Input","password":"range-secret"}]
            }
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/page-review", "", &request).unwrap();
        let model = page_review_model_with_edits(&state).unwrap();
        let protection = &model["worksheets"][0]["sheetProtection"];
        assert_eq!(protection["algorithmName"], "SHA-512");
        assert!(protection_runtime::verify_password(protection, Some("sheet-secret")).unwrap());
        assert!(!protection_runtime::verify_password(protection, Some("wrong")).unwrap());
        let range = &model["worksheets"][0]["protectedRanges"][0];
        assert!(protection_runtime::verify_password(range, Some("range-secret")).unwrap());
        let journal = serde_json::to_string(&state.native_page_review_edits).unwrap();
        assert!(!journal.contains("sheet-secret"));
        assert!(!journal.contains("range-secret"));

        let wrong = serde_json::to_vec(&json!({
            "op":"update", "password":"wrong",
            "patch":{"worksheets":[{"sheet":"Sheet1","sheetProtection":{"sort":false}}]}
        }))
        .unwrap();
        assert!(api_page_review(&mut state, &wrong).is_err());
        let right = serde_json::to_vec(&json!({
            "op":"update", "password":"sheet-secret",
            "patch":{"worksheets":[{"sheet":"Sheet1","sheetProtection":{"sort":false}}]}
        }))
        .unwrap();
        assert!(api_page_review(&mut state, &right).is_ok());
    }

    #[test]
    fn merged_cell_coordinates_always_resolve_to_the_top_left_anchor() {
        let ranges = vec![(3, 2, 5, 4), (10, 8, 10, 9)];
        assert_eq!(merged_anchor_in(&ranges, 3, 2), (3, 2));
        assert_eq!(merged_anchor_in(&ranges, 4, 3), (3, 2));
        assert_eq!(merged_anchor_in(&ranges, 5, 4), (3, 2));
        assert_eq!(merged_anchor_in(&ranges, 10, 9), (10, 8));
        assert_eq!(merged_anchor_in(&ranges, 6, 4), (6, 4));
    }

    fn test_zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::{Cursor, Write};
        let mut writer = zip::write::ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in parts {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn rich_text_api_resolves_merged_cell_and_promotes_plain_string_without_losing_phonetics() {
        let mut state = AppState::new();
        state.model.merge_cells_range(0, 3, 2, 4, 3).unwrap();
        state.model.set_user_input(0, 3, 2, "Text").unwrap();
        state.rich_text_xml.insert(
            (0, 3, 2),
            "<si><t>Text</t><phoneticPr fontId=\"2\" type=\"noConversion\"/></si>".to_string(),
        );
        let request = json!({
            "sheet": 0,
            "row": 4,
            "col": 3,
            "content": "Text",
            "runs": [
                {"text": "Te", "bold": true},
                {"text": "xt"}
            ]
        })
        .to_string();
        api_rich_text(&mut state, "", request.as_bytes()).unwrap();

        assert!(!state.rich_text.contains_key(&(0, 4, 3)));
        let runs = state.rich_text.get(&(0, 3, 2)).unwrap();
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            "Text"
        );
        assert!(runs[0].bold);
        assert_eq!(runs[0].font.as_deref(), Some("Inter"));
        assert_eq!(runs[0].size, Some(12.0));
        let raw = state.rich_text_xml.get(&(0, 3, 2)).unwrap();
        assert!(raw.contains("<phoneticPr fontId=\"2\" type=\"noConversion\"/>"));
        assert!(raw.contains("<b/>"));
        assert_eq!(state.model.get_cell_content(0, 3, 2).unwrap(), "Text");
    }

    #[test]
    fn rich_text_api_strictly_rejects_content_that_does_not_match_runs() {
        let mut state = AppState::new();
        let request = json!({
            "sheet": 0,
            "row": 1,
            "col": 1,
            "content": "AB",
            "runs": [{"text": "A"}]
        })
        .to_string();
        let error = match api_rich_text(&mut state, "", request.as_bytes()) {
            Ok(_) => panic!("mismatched rich-text content was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("do not concatenate"));
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "");
    }

    #[test]
    fn split_rich_run_reuses_its_source_template_without_shifting_later_runs() {
        let raw = concat!(
            "<si>",
            "<r><rPr><b/><color theme=\"5\" tint=\"-0.25\"/>",
            "<vertAlign val=\"superscript\"/></rPr><t>ABC</t></r>",
            "<r><rPr><color rgb=\"FF000000\"/><charset val=\"134\"/></rPr><t>D</t></r>",
            "</si>"
        );
        let document = roxmltree::Document::parse(raw).unwrap();
        let old = parse_rich_runs(document.root_element(), &Theme::default());
        assert_eq!(old[0].color, Color::Theme(5, -0.25));
        let mut first = old[0].clone();
        first.text = "A".to_string();
        first.bold = false;
        let mut second = old[0].clone();
        second.text = "BC".to_string();
        second.italic = true;
        let third = old[1].clone();
        let edited = build_rich_shared_item_xml(Some(raw), &old, &[first, second, third]).unwrap();

        assert_eq!(
            edited.matches("<vertAlign val=\"superscript\"/>").count(),
            2
        );
        assert_eq!(edited.matches("<charset val=\"134\"/>").count(), 1);
        assert!(edited.contains("<color rgb=\"FF000000\"/><charset val=\"134\"/></rPr><t>D</t>"));
        let edited_document = roxmltree::Document::parse(&edited).unwrap();
        let reparsed = parse_rich_runs(edited_document.root_element(), &Theme::default());
        assert_eq!(reparsed.len(), 3);
        assert_eq!(reparsed[0].color, Color::Theme(5, -0.25));
        assert_eq!(reparsed[1].color, Color::Theme(5, -0.25));
        assert_eq!(reparsed[2].color, Color::Rgb("#000000".to_string()));
    }

    #[test]
    fn rich_shared_string_export_import_roundtrip_preserves_theme_and_counts() {
        let input = test_zip(&[
            (
                "xl/workbook.xml",
                br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>"#,
            ),
            (
                "xl/sharedStrings.xml",
                br#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>AB</t></si></sst>"#,
            ),
        ]);
        let raw = concat!(
            "<si>",
            "<r><rPr><b/><color theme=\"4\" tint=\"0.2\"/></rPr><t>A</t></r>",
            "<r><rPr><i/><color rgb=\"FF112233\"/></rPr><t>B</t></r>",
            "</si>"
        );
        let rich = std::collections::HashMap::from([((0, 1, 1), raw.to_string())]);
        let exported = inject_rich_shared_strings(input, &rich).unwrap();
        let parts = unzip_test_parts(exported.clone());
        let shared = std::str::from_utf8(parts.get("xl/sharedStrings.xml").unwrap()).unwrap();
        assert!(shared.contains("count=\"1\""));
        assert!(shared.contains("uniqueCount=\"2\""));

        let (runs, raw_items) = parse_xlsx_rich_text(&exported, &Theme::default());
        let runs = runs.get(&(0, 1, 1)).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].color, Color::Theme(4, 0.2));
        assert_eq!(runs[1].color, Color::Rgb("#112233".to_string()));
        assert!(raw_items.get(&(0, 1, 1)).unwrap().contains("theme=\"4\""));
    }

    #[test]
    fn rich_shared_string_injection_recounts_duplicate_cell_references() {
        let input = test_zip(&[
            (
                "xl/workbook.xml",
                br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>0</v></c></row></sheetData></worksheet>"#,
            ),
            (
                "xl/sharedStrings.xml",
                br#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="99" uniqueCount="99"><si><t>AB</t></si></sst>"#,
            ),
        ]);
        let rich = std::collections::HashMap::from([(
            (0, 1, 1),
            "<si><r><rPr><b/></rPr><t>AB</t></r></si>".to_string(),
        )]);
        let exported = inject_rich_shared_strings(input, &rich).unwrap();
        let parts = unzip_test_parts(exported);
        let shared = std::str::from_utf8(parts.get("xl/sharedStrings.xml").unwrap()).unwrap();
        assert!(shared.contains(" count=\"2\""), "{shared}");
        assert!(shared.contains(" uniqueCount=\"2\""), "{shared}");
    }

    fn unzip_test_parts(bytes: Vec<u8>) -> std::collections::HashMap<String, Vec<u8>> {
        use std::io::{Cursor, Read};
        let mut archive = zip::read::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut result = std::collections::HashMap::new();
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).unwrap();
            if file.is_dir() {
                continue;
            }
            let mut content = Vec::new();
            file.read_to_end(&mut content).unwrap();
            result.insert(file.name().to_string(), content);
        }
        result
    }

    fn test_zip_from_map(parts: std::collections::HashMap<String, Vec<u8>>) -> Vec<u8> {
        use std::io::{Cursor, Write};
        let mut writer = zip::write::ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut parts: Vec<_> = parts.into_iter().collect();
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, bytes) in parts {
            writer.start_file(name, options).unwrap();
            writer.write_all(&bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn replace_test_cell_formula(
        xml: &mut String,
        reference: &str,
        formula: &str,
        cm: Option<&str>,
    ) {
        let document = roxmltree::Document::parse(xml).unwrap();
        let cell = document
            .descendants()
            .find(|n| n.is_element() && n.has_tag_name("c") && n.attribute("r") == Some(reference))
            .unwrap();
        let range = cell.range();
        let mut replacement = replace_cell_formula_fragment(&xml[range.clone()], formula);
        if let Some(cm) = cm {
            replacement = set_start_tag_attribute(&replacement, "cm", cm);
        }
        drop(document);
        xml.replace_range(range, &replacement);
    }

    const CHART_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>季度收入</a:t></a:r></a:p></c:rich></c:tx></c:title>
    <c:plotArea><c:barChart><c:ser><c:idx val="0"/><c:tx><c:v>收入</c:v></c:tx>
      <c:spPr><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></c:spPr>
      <c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Q1</c:v></c:pt><c:pt idx="1"><c:v>Q2</c:v></c:pt></c:strCache></c:strRef></c:cat>
      <c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>12</c:v></c:pt><c:pt idx="1"><c:v>18</c:v></c:pt></c:numCache></c:numRef></c:val>
    </c:ser></c:barChart></c:plotArea>
  </c:chart>
</c:chartSpace>"#;

    fn native_drawing_fixture() -> Vec<u8> {
        let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/drawings/drawing1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml"/><Override PartName="/xl/charts/chart1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/><Override PartName="/xl/diagrams/data1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramData+xml"/><Override PartName="/xl/diagrams/layout1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramLayout+xml"/><Override PartName="/xl/diagrams/quickStyle1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramStyle+xml"/><Override PartName="/xl/diagrams/colors1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramColors+xml"/><Override PartName="/xl/diagrams/drawing1.xml" ContentType="application/vnd.ms-office.drawingml.diagramDrawing+xml"/></Types>"#;
        let sheet = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><drawing r:id="rIdDrawing1"/></worksheet>"#;
        let sheet_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdDrawing1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#;
        let drawing = br#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram">
<xdr:oneCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:ext cx="952500" cy="476250"/><xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="NativePicture"/><xdr:cNvPicPr/></xdr:nvPicPr><xdr:blipFill><a:blip r:embed="rId2"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="952500" cy="476250"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></xdr:spPr></xdr:pic><xdr:clientData/></xdr:oneCellAnchor>
<xdr:twoCellAnchor editAs="twoCell"><xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>6</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>11</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="3" name="NativeChart"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm><a:off x="0" y="0"/><a:ext cx="4762500" cy="1905000"/></xdr:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId3"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor>
<xdr:absoluteAnchor><xdr:pos x="1905000" y="2857500"/><xdr:ext cx="1905000" cy="952500"/><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="4" name="GradientShape"/><xdr:cNvSpPr/></xdr:nvSpPr><xdr:spPr><a:xfrm rot="600000"><a:off x="0" y="0"/><a:ext cx="1905000" cy="952500"/></a:xfrm><a:prstGeom prst="roundRect"><a:avLst/></a:prstGeom><a:gradFill flip="x"><a:gsLst><a:gs pos="0"><a:schemeClr val="accent1"><a:tint val="25000"/><a:alpha val="85000"/></a:schemeClr></a:gs><a:gs pos="45000"><a:srgbClr val="33CC99"><a:alpha val="60000"/></a:srgbClr></a:gs><a:gs pos="100000"><a:schemeClr val="accent2"><a:shade val="70000"/><a:satMod val="120000"/></a:schemeClr></a:gs></a:gsLst><a:path path="circle"><a:fillToRect l="10000" t="10000" r="10000" b="10000"/></a:path></a:gradFill><a:effectLst><a:outerShdw blurRad="40000" dist="20000" dir="5400000"><a:srgbClr val="000000"><a:alpha val="35000"/></a:srgbClr></a:outerShdw><a:softEdge rad="12000"/></a:effectLst></xdr:spPr><xdr:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Gradient native</a:t></a:r></a:p></xdr:txBody></xdr:sp><xdr:clientData/></xdr:absoluteAnchor>
<xdr:twoCellAnchor><xdr:from><xdr:col>7</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>2</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>11</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>10</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="5" name="NativeSmartArt"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm><a:off x="0" y="0"/><a:ext cx="3810000" cy="1524000"/></xdr:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:relIds r:dm="rId4" r:lo="rId5" r:qs="rId6" r:cs="rId7"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor>
</xdr:wsDr>"#;
        let drawing_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData" Target="../diagrams/data1.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramLayout" Target="../diagrams/layout1.xml"/><Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramQuickStyle" Target="../diagrams/quickStyle1.xml"/><Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramColors" Target="../diagrams/colors1.xml"/><Relationship Id="rId8" Type="http://schemas.microsoft.com/office/2007/relationships/diagramDrawing" Target="../diagrams/drawing1.xml"/></Relationships>"#;
        test_zip(&[
            ("[Content_Types].xml", content_types), ("xl/worksheets/sheet1.xml", sheet),
            ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels), ("xl/drawings/drawing1.xml", drawing),
            ("xl/drawings/_rels/drawing1.xml.rels", drawing_rels), ("xl/media/image1.png", b"native-picture-bytes"),
            ("xl/charts/chart1.xml", CHART_XML.as_bytes()),
            ("xl/diagrams/data1.xml", br#"<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram" xmlns:x="urn:fixture" layoutId="urn:fixture:hierarchy"><dgm:ptLst><dgm:pt modelId="ROOT" type="node" x:keep="root"><dgm:prSet/><dgm:t><a:p><a:r><a:rPr b="1"/><a:t>SmartArt root</a:t></a:r></a:p></dgm:t></dgm:pt><dgm:pt modelId="CHILD" type="node"><dgm:prSet/><dgm:t><a:p><a:r><a:t>SmartArt child</a:t></a:r></a:p></dgm:t></dgm:pt></dgm:ptLst><dgm:cxnLst><dgm:cxn modelId="CXN1" type="parOf" srcId="ROOT" destId="CHILD" srcOrd="0" destOrd="0" x:keep="connection"/></dgm:cxnLst><dgm:extLst><x:opaque value="byte-exact"/><a:ext uri="{UNICELL-DIAGRAM-CACHE}"><dsp:dataModelExt relId="rId8" minVer="http://schemas.openxmlformats.org/drawingml/2006/diagram"/></a:ext></dgm:extLst></dgm:dataModel>"#),
            ("xl/diagrams/layout1.xml", b"layout-byte-exact"), ("xl/diagrams/quickStyle1.xml", b"quick-style-byte-exact"),
            ("xl/diagrams/colors1.xml", b"diagram-colors-byte-exact"),
            ("xl/diagrams/drawing1.xml", b"diagram-drawing-byte-exact"),
        ])
    }

    fn native_drawing_fixture_with_cache_dependency() -> Vec<u8> {
        let mut parts = unzip_test_parts(native_drawing_fixture());
        parts.insert(
            "xl/diagrams/_rels/drawing1.xml.rels".to_string(),
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdCacheImage" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/cache-image.png"/></Relationships>"#.to_vec(),
        );
        parts.insert(
            "xl/media/cache-image.png".to_string(),
            b"diagram-cache-image".to_vec(),
        );
        test_zip_from_map(parts)
    }

    fn drawing_exports(objects: &[Value], include_generated_picture: bool) -> Vec<Value> {
        let mut result: Vec<Value> = objects.iter().map(|object| json!({
            "id": object["id"].clone(), "type": object["type"].clone(), "mode": object["mode"].clone(),
            "sheet": object["sheet"].clone(), "r": object["r"].clone(), "c": object["c"].clone(),
            "x": object["x"].clone(), "y": object["y"].clone(), "w": object["w"].clone(), "h": object["h"].clone(),
            "nativeDrawing": object["config"]["nativeDrawing"].clone(),
        })).collect();
        if include_generated_picture {
            result.push(json!({
                "id":"generated-picture", "type":"image", "mode":"abs", "sheet":0,
                "r":1, "c":1, "x":20, "y":20, "w":1, "h":1,
                "png":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
            }));
        }
        result
    }

    #[test]
    fn chart_xml_becomes_responsive_iframe_document_with_theme_color() {
        let theme = Theme::default();
        let (html, kind, title) = chart_xml_to_html(CHART_XML, &theme, None).expect("chart html");
        assert_eq!(kind, "bar");
        assert_eq!(title, "季度收入");
        assert!(html.contains("ResizeObserver"));
        assert!(html.contains("季度收入"));
        assert!(html.contains(&theme.accent2));
        assert!(html.contains("Q1"));
    }

    #[test]
    fn drawing_and_chart_first_frame_share_ordered_theme_color_resolution() {
        let mut theme = Theme::default();
        theme.accent1 = "#808080".to_string();
        let chart = r#"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><c:chart><c:plotArea><c:barChart><c:ser><c:idx val="0"/><c:order val="0"/><c:spPr><a:solidFill><a:schemeClr val="accent1"><a:tint val="50000"/><a:shade val="50000"/></a:schemeClr></a:solidFill></c:spPr><c:cat><c:strLit><c:ptCount val="1"/><c:pt idx="0"><c:v>A</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:ptCount val="1"/><c:pt idx="0"><c:v>1</c:v></c:pt></c:numLit></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let document = roxmltree::Document::parse(chart).unwrap();
        let series = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "ser")
            .unwrap();
        let color = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "schemeClr")
            .unwrap();
        assert_eq!(chart_series_color(series, &theme, "#010203"), "#606060");
        assert_eq!(drawing_color(color, &theme), ("#606060".to_string(), 1.0));
        let (html, _, _) = chart_xml_to_html(chart, &theme, None).unwrap();
        assert!(html.contains("#606060"));

        let unknown = r#"<c:barChart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><c:ser><c:spPr><a:solidFill><a:prstClr val="vendorMysteryColour"/></a:solidFill></c:spPr></c:ser></c:barChart>"#;
        let unknown_document = roxmltree::Document::parse(unknown).unwrap();
        let unknown_series = unknown_document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "ser")
            .unwrap();
        assert_eq!(
            chart_series_color(unknown_series, &theme, "#4472C4"),
            "#808080"
        );
    }

    #[test]
    fn imported_theme_part_is_restored_byte_exact() {
        let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/></Types>"#;
        let original_theme = br#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Custom Theme"><a:themeElements><a:clrScheme name="Custom"><a:dk1><a:srgbClr val="112233"/></a:dk1><a:lt1><a:srgbClr val="FDFCFB"/></a:lt1><a:accent1><a:srgbClr val="00BAF5"/></a:accent1></a:clrScheme><a:fontScheme name="Keep Fonts"><a:majorFont><a:latin typeface="Cambria"/></a:majorFont></a:fontScheme><a:fmtScheme name="Keep Effects"><a:fillStyleLst><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></a:fillStyleLst></a:fmtScheme></a:themeElements><a:extLst><a:ext uri="keep-theme-extension"/></a:extLst></a:theme>"#;
        let generated_theme = br#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Generated Default"/>"#;
        let original = test_zip(&[
            ("[Content_Types].xml", content_types),
            ("xl/theme/theme1.xml", original_theme),
        ]);
        let generated = test_zip(&[
            ("[Content_Types].xml", content_types),
            ("xl/theme/theme1.xml", generated_theme),
        ]);
        let mut state = AppState::new();
        state.source_ooxml = Some(snapshot_opc_package(&original).expect("snapshot"));
        let output = unzip_test_parts(
            restore_preserved_ooxml(&state, generated).expect("restore preserved theme"),
        );
        assert_eq!(output["xl/theme/theme1.xml"], original_theme);
        assert!(opc_part_is_preservable("xl/theme/theme1.xml"));
    }

    #[test]
    fn nonstandard_imported_theme_replaces_generated_theme_as_a_singleton() {
        let original_content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/theme/theme9.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/></Types>"#;
        let original_workbook = br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets/></workbook>"#;
        let original_workbook_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdTheme9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="theme/theme9.xml"/></Relationships>"#;
        let original_theme = br#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Custom Theme 9"><a:themeElements><a:clrScheme name="Custom"><a:dk1><a:srgbClr val="112233"/></a:dk1><a:lt1><a:srgbClr val="FDFCFB"/></a:lt1><a:accent1><a:srgbClr val="00BAF5"/></a:accent1></a:clrScheme><a:fontScheme name="Keep Fonts"><a:majorFont><a:latin typeface="Cambria"/></a:majorFont></a:fontScheme><a:fmtScheme name="Keep Effects"><a:fillStyleLst><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></a:fillStyleLst></a:fmtScheme></a:themeElements></a:theme>"#;
        let original = test_zip(&[
            ("[Content_Types].xml", original_content_types),
            ("xl/workbook.xml", original_workbook),
            ("xl/_rels/workbook.xml.rels", original_workbook_rels),
            ("xl/theme/theme9.xml", original_theme),
        ]);
        let snapshot = snapshot_opc_package(&original).expect("snapshot");
        let imported_theme = parse_xlsx_theme(&snapshot).expect("relationship-selected theme9");
        assert_eq!(imported_theme.accent1, "#00BAF5");
        assert_eq!(imported_theme.dk1, "#112233");
        let mut state = AppState::new();
        state.model.set_theme(imported_theme);
        let descriptor_theme = drawing_theme_model(&state.model.get_theme());
        assert_eq!(descriptor_theme["accent1"], "#00BAF5");
        assert_eq!(descriptor_theme["dk1"], "#112233");
        state.source_ooxml = Some(snapshot);

        let output = unzip_test_parts(
            model_to_preserved_xlsx_bytes(&state).expect("full preserved xlsx export"),
        );
        assert_eq!(output["xl/theme/theme9.xml"], original_theme);
        assert!(!output.contains_key("xl/theme/theme1.xml"));

        let rels_xml = std::str::from_utf8(&output["xl/_rels/workbook.xml.rels"]).unwrap();
        let rels_doc = roxmltree::Document::parse(rels_xml).unwrap();
        let theme_relationships: Vec<_> = rels_doc
            .descendants()
            .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
            .filter(|node| {
                node.attribute("Type")
                    .map(|value| value.ends_with("/theme"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(theme_relationships.len(), 1);
        assert_eq!(
            theme_relationships[0].attribute("Target"),
            Some("theme/theme9.xml")
        );

        let types_xml = std::str::from_utf8(&output["[Content_Types].xml"]).unwrap();
        let types_doc = roxmltree::Document::parse(types_xml).unwrap();
        let theme_overrides: Vec<_> = types_doc
            .descendants()
            .filter(|node| node.is_element() && node.has_tag_name("Override"))
            .filter(|node| {
                node.attribute("PartName")
                    .map(|value| value.starts_with("/xl/theme/"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(theme_overrides.len(), 1);
        assert_eq!(
            theme_overrides[0].attribute("PartName"),
            Some("/xl/theme/theme9.xml")
        );
    }

    #[test]
    fn two_cell_chart_anchor_imports_as_html_object_and_keeps_xml() {
        let model = UserModel::new_empty("Book1", "en", "UTC", "en").unwrap();
        let drawing = r#"<twoCellAnchor><from><col>0</col><colOff>0</colOff><row>0</row><rowOff>0</rowOff></from><to><col>2</col><colOff>0</colOff><row>4</row><rowOff>0</rowOff></to><graphicFrame><a:graphic><a:graphicData><chart r:id="rId1"/></a:graphicData></a:graphic></graphicFrame></twoCellAnchor>"#;
        let mut rels = std::collections::HashMap::new();
        rels.insert("rId1".to_string(), "/xl/charts/chart1.xml".to_string());
        let mut files = std::collections::HashMap::new();
        files.insert(
            "xl/charts/chart1.xml".to_string(),
            CHART_XML.as_bytes().to_vec(),
        );
        let objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &model,
        );
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0]["type"], "html");
        assert_eq!(objects[0]["config"]["source"], "xlsx-chart");
        assert_eq!(objects[0]["config"]["nativeDrawing"]["kind"], "chart");
        assert_eq!(
            objects[0]["config"]["nativeDrawing"]["theme"]["accent1"],
            model.get_theme().accent1
        );
        assert_eq!(
            objects[0]["config"]["nativeDrawing"]["theme"]["folHlink"],
            model.get_theme().fol_hlink
        );
        assert!(
            objects[0]["config"]["excelChartXml"]
                .as_str()
                .unwrap()
                .contains("barChart")
        );
        assert!(objects[0]["w"].as_i64().unwrap() > 96);
        assert!(objects[0]["h"].as_i64().unwrap() > 50);
    }

    #[test]
    fn native_chart_shape_smartart_and_gradient_round_trip_without_rasterizing() {
        let original = native_drawing_fixture();
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let drawing = std::str::from_utf8(&files["xl/drawings/drawing1.xml"]).unwrap();
        let rels = parse_rels_map(
            std::str::from_utf8(&files["xl/drawings/_rels/drawing1.xml.rels"]).unwrap(),
        );
        let mut state = AppState::new();
        let mut objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &state.model,
        );
        assert_eq!(objects.len(), 4);
        assert!(
            objects
                .iter()
                .any(|object| object["config"]["nativeDrawing"]["kind"] == "picture")
        );
        assert!(
            objects
                .iter()
                .any(|object| object["config"]["nativeDrawing"]["kind"] == "shape")
        );
        assert!(
            objects
                .iter()
                .any(|object| object["config"]["nativeDrawing"]["kind"] == "smartart")
        );
        let imported_chart = objects
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "chart")
            .unwrap();
        assert_eq!(
            imported_chart["config"]["nativeDrawing"]["model"]["title"],
            "季度收入"
        );
        assert_eq!(
            imported_chart["config"]["nativeDrawing"]["model"]["series"][0]["name"],
            "收入"
        );
        let imported_shape = objects
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "shape")
            .unwrap();
        assert_eq!(
            imported_shape["config"]["nativeDrawing"]["model"]["text"], "Gradient native",
            "{}",
            imported_shape["config"]["nativeDrawing"]["model"]
        );
        assert_eq!(
            imported_shape["config"]["nativeDrawing"]["model"]["geometry"],
            "roundRect"
        );
        let imported_smartart = objects
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "smartart")
            .unwrap();
        assert_eq!(
            imported_smartart["config"]["nativeDrawing"]["model"]["nodes"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            imported_smartart["config"]["nativeDrawing"]["model"]["nodes"][1]["parentId"],
            "ROOT"
        );
        let chart = objects
            .iter_mut()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "chart")
            .unwrap();
        chart["x"] = json!(json_number(chart, "x", 0.0) + 175.0);
        chart["y"] = json!(json_number(chart, "y", 0.0) + 63.0);
        state.objects.insert(0, objects.clone());
        state.source_ooxml = Some(snapshot);

        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
        ]);
        let injected = inject_drawings(generated, &drawing_exports(&objects, true), &[]).unwrap();
        let output = unzip_test_parts(restore_preserved_ooxml(&state, injected).unwrap());
        assert!(output.contains_key("xl/drawings/unicellDrawing1.xml"));
        assert!(!output.contains_key("xl/drawings/drawing1.xml"));
        assert_eq!(output["xl/charts/chart1.xml"], CHART_XML.as_bytes());
        assert_eq!(output["xl/media/image1.png"], b"native-picture-bytes");
        assert!(
            output
                .keys()
                .any(|name| name.starts_with("xl/media/unicellImage"))
        );
        assert_eq!(output["xl/diagrams/layout1.xml"], b"layout-byte-exact");

        let composed = std::str::from_utf8(&output["xl/drawings/unicellDrawing1.xml"]).unwrap();
        assert_eq!(drawing_anchor_slices(composed).len(), 5);
        assert!(composed.contains("<a:gradFill flip=\"x\">"));
        assert!(composed.contains("<a:gs pos=\"45000\">"));
        assert!(composed.contains("<a:path path=\"circle\">"));
        assert!(composed.contains("<a:outerShdw"));
        assert!(composed.contains("<a:softEdge"));
        assert!(composed.contains("<dgm:relIds"));
        assert!(composed.contains("NativeChart"));
        let composed_document = roxmltree::Document::parse(composed).unwrap();
        let ids: Vec<i64> = composed_document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "cNvPr")
            .filter_map(|node| node.attribute("id").and_then(|value| value.parse().ok()))
            .collect();
        let unique: std::collections::HashSet<i64> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len());

        let drawing_rels =
            std::str::from_utf8(&output["xl/drawings/_rels/unicellDrawing1.xml.rels"]).unwrap();
        let drawing_rels_document = roxmltree::Document::parse(drawing_rels).unwrap();
        let targets: Vec<&str> = drawing_rels_document
            .descendants()
            .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
            .filter_map(|node| node.attribute("Target"))
            .collect();
        assert!(targets.contains(&"../charts/chart1.xml"));
        assert!(targets.contains(&"../diagrams/data1.xml"));
        assert!(targets.iter().any(|target| target.contains("unicellImage")));
        let content_types = std::str::from_utf8(&output["[Content_Types].xml"]).unwrap();
        assert!(content_types.contains("/xl/charts/chart1.xml"));
        assert!(content_types.contains("/xl/diagrams/data1.xml"));
    }

    #[test]
    fn native_deep_chart_shape_and_smartart_edits_round_trip() {
        let original = native_drawing_fixture();
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let drawing = std::str::from_utf8(&files["xl/drawings/drawing1.xml"]).unwrap();
        let rels = parse_rels_map(
            std::str::from_utf8(&files["xl/drawings/_rels/drawing1.xml.rels"]).unwrap(),
        );
        let mut state = AppState::new();
        let mut objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &state.model,
        );

        let chart = objects
            .iter_mut()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "chart")
            .unwrap();
        chart["config"]["nativeDrawing"]["edits"]["chart"] = json!({
            "title":"Edited native chart",
            "legend":{"show":true,"position":"bottom"},
            "series":[{
                "name":"Edited revenue",
                "categoryFormula":"Sheet1!$A$5:$A$7",
                "valueFormula":"Sheet1!$B$5:$B$7",
                "categories":["Q1","Q2","Q3"],
                "values":[21,null,34],
                "color":"scheme:accent3"
            }]
        });
        let shape = objects
            .iter_mut()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "shape")
            .unwrap();
        shape["config"]["nativeDrawing"]["edits"]["shape"] = json!({
            "text":"Edited shape text",
            "rotation":27.5,
            "fill":{"stops":[
                {"position":0.0,"color":"accent4","alpha":0.8},
                {"position":0.5,"color":"#123456","alpha":0.65},
                {"position":1.0,"color":"accent2","alpha":1.0}
            ]},
            "line":{"color":"accent5","alpha":0.9,"width":2.5,"dash":"dashDot"},
            "effects":{"shadow":{"enabled":true,"distance":8.0,"blur":5.0,"angle":30.0},"softEdge":1.5}
        });
        let smartart = objects
            .iter_mut()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "smartart")
            .unwrap();
        smartart["config"]["nativeDrawing"]["edits"]["smartart"] = json!({
            "nodes":[
                {"id":"ROOT","parentId":null,"text":"Edited root","order":0,"kind":"node"},
                {"id":"CHILD","parentId":"ROOT","text":"Edited child","order":0,"kind":"asst"},
                {"id":"new-grandchild","parentId":"CHILD","text":"New grandchild","order":0,"kind":"node"}
            ]
        });

        state.objects.insert(0, objects.clone());
        state.source_ooxml = Some(snapshot);
        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
        ]);
        let injected = inject_drawings(generated, &drawing_exports(&objects, false), &[]).unwrap();
        let output = unzip_test_parts(restore_preserved_ooxml(&state, injected).unwrap());

        let chart_xml = std::str::from_utf8(&output["xl/charts/chart1.xml"]).unwrap();
        let chart_model = native_chart_edit::parse_chart_model(chart_xml);
        assert_eq!(chart_model["title"], "Edited native chart");
        assert_eq!(chart_model["legend"]["position"], "bottom");
        assert_eq!(chart_model["series"][0]["name"], "Edited revenue");
        assert_eq!(
            chart_model["series"][0]["values"],
            json!([21.0, null, 34.0])
        );
        assert_eq!(chart_model["series"][0]["color"], "scheme:accent3");

        let composed = std::str::from_utf8(&output["xl/drawings/unicellDrawing1.xml"]).unwrap();
        let shape_anchor = drawing_anchor_slices(composed)
            .into_iter()
            .map(|(start, end, _)| &composed[start..end])
            .find(|anchor| anchor.contains("GradientShape"))
            .unwrap();
        let shape_model = native_shape_edit::parse_shape_model(
            &drawing_fragment_document_with_source(shape_anchor, Some(composed)),
        );
        assert_eq!(shape_model["text"], "Edited shape text");
        assert_eq!(shape_model["rotation"], 27.5);
        assert_eq!(shape_model["fill"]["stops"].as_array().unwrap().len(), 3);
        assert_eq!(shape_model["line"]["width"], 2.5);
        assert_eq!(shape_model["line"]["dash"], "dashDot");
        assert_eq!(shape_model["effects"]["shadow"]["distance"], 8.0);
        assert!(shape_anchor.contains("flip=\"x\""));
        assert!(shape_anchor.contains("<a:path path=\"circle\""));

        let data_xml = std::str::from_utf8(&output["xl/diagrams/data1.xml"]).unwrap();
        let smart_model = native_smartart_edit::parse_smartart_model(data_xml);
        assert_eq!(smart_model["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(smart_model["nodes"][0]["text"], "Edited root");
        assert!(
            smart_model["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["text"] == "New grandchild")
        );
        assert!(data_xml.contains("value=\"byte-exact\""));
        assert!(data_xml.contains("x:keep=\"root\""));
        assert!(!data_xml.contains("dataModelExt"));
        let edited_drawing_rels =
            std::str::from_utf8(&output["xl/drawings/_rels/unicellDrawing1.xml.rels"]).unwrap();
        assert!(!edited_drawing_rels.contains("/diagramDrawing"));
        assert!(!output.contains_key("xl/diagrams/drawing1.xml"));

        let composed_rels = parse_rels_map(
            std::str::from_utf8(&output["xl/drawings/_rels/unicellDrawing1.xml.rels"]).unwrap(),
        );
        let reparsed = parse_drawing_pics(
            composed,
            &composed_rels,
            "xl/drawings/unicellDrawing1.xml",
            &output
                .iter()
                .map(|(name, bytes)| (name.clone(), bytes.clone()))
                .collect(),
            0,
            &state.model,
        );
        let reparsed_chart = reparsed
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "chart")
            .unwrap();
        assert_eq!(
            reparsed_chart["config"]["nativeDrawing"]["model"]["title"],
            "Edited native chart"
        );
        let reparsed_shape = reparsed
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "shape")
            .unwrap();
        assert_eq!(
            reparsed_shape["config"]["nativeDrawing"]["model"]["text"],
            "Edited shape text"
        );
        let reparsed_smartart = reparsed
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "smartart")
            .unwrap();
        assert_eq!(
            reparsed_smartart["config"]["nativeDrawing"]["model"]["nodes"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn noop_smartart_edit_preserves_the_original_diagram_cache() {
        let original = native_drawing_fixture();
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let drawing = std::str::from_utf8(&files["xl/drawings/drawing1.xml"]).unwrap();
        let rels = parse_rels_map(
            std::str::from_utf8(&files["xl/drawings/_rels/drawing1.xml.rels"]).unwrap(),
        );
        let mut state = AppState::new();
        let mut objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &state.model,
        );
        let smartart = objects
            .iter_mut()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "smartart")
            .unwrap();
        let parsed_model = smartart["config"]["nativeDrawing"]["model"].clone();
        smartart["config"]["nativeDrawing"]["edits"]["smartart"] = parsed_model;
        state.objects.insert(0, objects.clone());
        state.source_ooxml = Some(snapshot);

        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
        ]);
        let injected = inject_drawings(generated, &drawing_exports(&objects, false), &[]).unwrap();
        let output = unzip_test_parts(restore_preserved_ooxml(&state, injected).unwrap());
        let data_xml = std::str::from_utf8(&output["xl/diagrams/data1.xml"]).unwrap();
        assert!(data_xml.contains("dsp:dataModelExt"));
        let drawing_rels =
            std::str::from_utf8(&output["xl/drawings/_rels/unicellDrawing1.xml.rels"]).unwrap();
        assert!(drawing_rels.contains("/diagramDrawing"));
        assert_eq!(
            output["xl/diagrams/drawing1.xml"],
            b"diagram-drawing-byte-exact"
        );
        let content_types = std::str::from_utf8(&output["[Content_Types].xml"]).unwrap();
        assert!(content_types.contains("/xl/diagrams/drawing1.xml"));
    }

    #[test]
    fn editing_one_of_two_smartarts_that_share_data_forks_only_the_edited_data_part() {
        let mut original_parts = unzip_test_parts(native_drawing_fixture());
        let original_drawing =
            String::from_utf8(original_parts["xl/drawings/drawing1.xml"].clone()).unwrap();
        let shared_anchor = drawing_anchor_slices(&original_drawing)
            .into_iter()
            .map(|(start, end, _)| &original_drawing[start..end])
            .find(|anchor| anchor.contains("NativeSmartArt"))
            .unwrap();
        let shared_anchor =
            set_native_non_visual_id(shared_anchor, 15).replace("NativeSmartArt", "SharedSmartArt");
        let mut duplicated_drawing = original_drawing.clone();
        let insert_at = duplicated_drawing.rfind("</xdr:wsDr>").unwrap();
        duplicated_drawing.insert_str(insert_at, &shared_anchor);
        original_parts.insert(
            "xl/drawings/drawing1.xml".to_string(),
            duplicated_drawing.into_bytes(),
        );
        let original = test_zip_from_map(original_parts);
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let drawing = std::str::from_utf8(&files["xl/drawings/drawing1.xml"]).unwrap();
        let rels = parse_rels_map(
            std::str::from_utf8(&files["xl/drawings/_rels/drawing1.xml.rels"]).unwrap(),
        );
        let mut state = AppState::new();
        let mut objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &state.model,
        );
        let edited = objects
            .iter_mut()
            .find(|object| {
                object["config"]["nativeDrawing"]["kind"] == "smartart"
                    && object["config"]["nativeDrawing"]["nonVisualId"] == "5"
            })
            .unwrap();
        edited["config"]["nativeDrawing"]["edits"]["smartart"] =
            json!({"updates":[{"id":"ROOT","text":"Fork-only root"}]});
        state.objects.insert(0, objects.clone());
        state.source_ooxml = Some(snapshot);

        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
        ]);
        let injected = inject_drawings(generated, &drawing_exports(&objects, false), &[]).unwrap();
        let output = unzip_test_parts(restore_preserved_ooxml(&state, injected).unwrap());
        let composed_path = "xl/drawings/unicellDrawing1.xml";
        let composed = std::str::from_utf8(&output[composed_path]).unwrap();
        let drawing_rels = parse_rels_map(
            std::str::from_utf8(&output["xl/drawings/_rels/unicellDrawing1.xml.rels"]).unwrap(),
        );
        let data_targets: Vec<String> = drawing_anchor_slices(composed)
            .into_iter()
            .map(|(start, end, _)| &composed[start..end])
            .filter(|anchor| anchor.contains("<dgm:relIds"))
            .map(|anchor| drawing_any_element_attr(anchor, "relIds", "r:dm").unwrap())
            .map(|id| resolve_rel_path(&path_dir(composed_path), &drawing_rels[&id]))
            .collect();
        assert_eq!(data_targets.len(), 2);
        assert_ne!(data_targets[0], data_targets[1]);
        let original_data = data_targets
            .iter()
            .find(|target| target.as_str() == "xl/diagrams/data1.xml")
            .unwrap();
        let forked_data = data_targets
            .iter()
            .find(|target| target.contains("unicellData"))
            .unwrap();
        let original_xml = std::str::from_utf8(&output[original_data]).unwrap();
        let forked_xml = std::str::from_utf8(&output[forked_data]).unwrap();
        assert!(original_xml.contains("SmartArt root"));
        assert!(!original_xml.contains("Fork-only root"));
        assert!(original_xml.contains("dsp:dataModelExt"));
        assert!(forked_xml.contains("Fork-only root"));
        assert!(!forked_xml.contains("dataModelExt"));
        assert_eq!(
            output["xl/diagrams/drawing1.xml"],
            b"diagram-drawing-byte-exact"
        );
    }

    #[test]
    fn cross_sheet_shared_smartart_data_is_counted_globally_before_editing() {
        let mut original_parts = unzip_test_parts(native_drawing_fixture());
        original_parts.insert(
            "xl/worksheets/sheet2.xml".to_string(),
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><drawing r:id="rIdDrawing2"/></worksheet>"#.to_vec(),
        );
        original_parts.insert(
            "xl/worksheets/_rels/sheet2.xml.rels".to_string(),
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdDrawing2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing2.xml"/></Relationships>"#.to_vec(),
        );
        original_parts.insert(
            "xl/drawings/drawing2.xml".to_string(),
            original_parts["xl/drawings/drawing1.xml"].clone(),
        );
        original_parts.insert(
            "xl/drawings/_rels/drawing2.xml.rels".to_string(),
            original_parts["xl/drawings/_rels/drawing1.xml.rels"].clone(),
        );
        let mut original_content_types =
            String::from_utf8(original_parts["[Content_Types].xml"].clone()).unwrap();
        original_content_types = original_content_types.replace(
            "</Types>",
            "<Override PartName=\"/xl/worksheets/sheet2.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/><Override PartName=\"/xl/drawings/drawing2.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.drawing+xml\"/></Types>",
        );
        original_parts.insert(
            "[Content_Types].xml".to_string(),
            original_content_types.into_bytes(),
        );
        let original = test_zip_from_map(original_parts);
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let mut state = AppState::new();
        state.model.new_sheet().unwrap();
        let parse_sheet_objects = |drawing_path: &str, sheet: u32| {
            let drawing = std::str::from_utf8(&files[drawing_path]).unwrap();
            let rels_path = drawing_relationships_path(drawing_path);
            let rels = parse_rels_map(std::str::from_utf8(&files[&rels_path]).unwrap());
            parse_drawing_pics(drawing, &rels, drawing_path, &files, sheet, &state.model)
        };
        let mut sheet1_objects = parse_sheet_objects("xl/drawings/drawing1.xml", 0);
        let sheet2_objects = parse_sheet_objects("xl/drawings/drawing2.xml", 1);
        let edited = sheet1_objects
            .iter_mut()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "smartart")
            .unwrap();
        edited["config"]["nativeDrawing"]["edits"]["smartart"] =
            json!({"updates":[{"id":"ROOT","text":"Cross-sheet fork"}]});
        let mut all_objects = sheet1_objects.clone();
        all_objects.extend(sheet2_objects.clone());
        state.objects.insert(0, sheet1_objects);
        state.objects.insert(1, sheet2_objects);
        state.source_ooxml = Some(snapshot);

        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
            ("xl/worksheets/sheet2.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
        ]);
        let injected =
            inject_drawings(generated, &drawing_exports(&all_objects, false), &[]).unwrap();
        let output = unzip_test_parts(restore_preserved_ooxml(&state, injected).unwrap());
        let smartart_data_target = |drawing_path: &str| {
            let drawing = std::str::from_utf8(&output[drawing_path]).unwrap();
            let rels_path = drawing_relationships_path(drawing_path);
            let rels = parse_rels_map(std::str::from_utf8(&output[&rels_path]).unwrap());
            let anchor = drawing_anchor_slices(drawing)
                .into_iter()
                .map(|(start, end, _)| &drawing[start..end])
                .find(|anchor| anchor.contains("<dgm:relIds"))
                .unwrap();
            let id = drawing_any_element_attr(anchor, "relIds", "r:dm").unwrap();
            resolve_rel_path(&path_dir(drawing_path), &rels[&id])
        };
        let edited_data = smartart_data_target("xl/drawings/unicellDrawing1.xml");
        let untouched_data = smartart_data_target("xl/drawings/unicellDrawing2.xml");
        assert!(edited_data.contains("unicellData"));
        assert_eq!(untouched_data, "xl/diagrams/data1.xml");
        let edited_xml = std::str::from_utf8(&output[&edited_data]).unwrap();
        let untouched_xml = std::str::from_utf8(&output[&untouched_data]).unwrap();
        assert!(edited_xml.contains("Cross-sheet fork"));
        assert!(!edited_xml.contains("dataModelExt"));
        assert!(!untouched_xml.contains("Cross-sheet fork"));
        assert!(untouched_xml.contains("dsp:dataModelExt"));
        let edited_rels =
            std::str::from_utf8(&output["xl/drawings/_rels/unicellDrawing1.xml.rels"]).unwrap();
        let untouched_rels =
            std::str::from_utf8(&output["xl/drawings/_rels/unicellDrawing2.xml.rels"]).unwrap();
        assert!(!edited_rels.contains("/diagramDrawing"));
        assert!(untouched_rels.contains("/diagramDrawing"));
    }

    #[test]
    fn native_clone_creation_ids_are_unique_deterministic_and_schema_shaped() {
        let first = native_clone_creation_id("clone-a");
        let second = native_clone_creation_id("clone-b");
        assert_eq!(first, native_clone_creation_id("clone-a"));
        assert_ne!(first, second);
        assert_eq!(first.len(), 38);
        assert!(first.starts_with('{') && first.ends_with('}'));
        let anchor = r#"<xdr:twoCellAnchor><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="2"><a:extLst><a:ext><a16:creationId id="{OLD}"/></a:ext></a:extLst></xdr:cNvPr></xdr:nvGraphicFramePr></xdr:graphicFrame></xdr:twoCellAnchor>"#;
        let updated = set_native_creation_id(anchor, &first);
        assert!(updated.contains(&format!("<a16:creationId id=\"{first}\"/>")));
        assert!(!updated.contains("{OLD}"));
    }

    #[test]
    fn native_relationship_id_remapping_is_collision_safe() {
        let id_map = std::collections::HashMap::from([
            ("rId4".to_string(), "rId2".to_string()),
            ("rId2".to_string(), "rId3".to_string()),
        ]);
        let anchor = remap_drawing_relationship_attributes(
            r#"<dgm:relIds r:dm="rId4" r:lo='rId2' r:qs="rIdKeep"/>"#.to_string(),
            &id_map,
        );
        assert!(anchor.contains("r:dm=\"rId2\""));
        assert!(anchor.contains("r:lo='rId3'"));
        assert!(anchor.contains("r:qs=\"rIdKeep\""));

        let data = remap_diagram_data_relationship(
            r#"<root xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram" xmlns:x="urn:other"><dsp:dataModelExt relId="rId4"/><x:dataModelExt relId='rId2'/></root>"#.to_string(),
            &id_map,
        );
        assert!(data.contains("relId=\"rId2\""));
        assert!(data.contains("<x:dataModelExt relId='rId2'/>"));
    }

    #[test]
    fn native_chart_and_smartart_clones_have_independent_editable_parts() {
        let original = native_drawing_fixture();
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let drawing = std::str::from_utf8(&files["xl/drawings/drawing1.xml"]).unwrap();
        let rels = parse_rels_map(
            std::str::from_utf8(&files["xl/drawings/_rels/drawing1.xml.rels"]).unwrap(),
        );
        let mut state = AppState::new();
        let mut objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &state.model,
        );

        let source_chart = objects
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "chart")
            .unwrap()
            .clone();
        let mut chart_clone = source_chart.clone();
        chart_clone["id"] = json!("native-chart-clone");
        chart_clone["x"] = json!(json_number(&source_chart, "x", 0.0) + 420.0);
        chart_clone["config"]["nativeDrawing"]["clone"] = json!(true);
        chart_clone["config"]["nativeDrawing"]["cloneOfToken"] =
            source_chart["config"]["nativeDrawing"]["token"].clone();
        chart_clone["config"]["nativeDrawing"]["token"] = json!("native-clone-chart-test");
        chart_clone["config"]["nativeDrawing"]["edits"]["chart"] =
            json!({"title":"Clone-only chart title"});
        objects.push(chart_clone);

        let source_smartart = objects
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "smartart")
            .unwrap()
            .clone();
        let mut smartart_clone = source_smartart.clone();
        smartart_clone["id"] = json!("native-smartart-clone");
        smartart_clone["y"] = json!(json_number(&source_smartart, "y", 0.0) + 260.0);
        smartart_clone["config"]["nativeDrawing"]["clone"] = json!(true);
        smartart_clone["config"]["nativeDrawing"]["cloneOfToken"] =
            source_smartart["config"]["nativeDrawing"]["token"].clone();
        smartart_clone["config"]["nativeDrawing"]["token"] = json!("native-clone-smartart-test");
        smartart_clone["config"]["nativeDrawing"]["edits"]["smartart"] =
            json!({"updates":[{"id":"ROOT","text":"Clone-only SmartArt root"}]});
        objects.push(smartart_clone);

        state.objects.insert(0, objects.clone());
        state.source_ooxml = Some(snapshot);
        let make_generated = || {
            test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
        ])
        };
        let export_once = || {
            let injected =
                inject_drawings(make_generated(), &drawing_exports(&objects, false), &[]).unwrap();
            unzip_test_parts(restore_preserved_ooxml(&state, injected).unwrap())
        };
        let output = export_once();
        let composed_path = "xl/drawings/unicellDrawing1.xml";
        let composed = std::str::from_utf8(&output[composed_path]).unwrap();
        let composed_document = roxmltree::Document::parse(composed).unwrap();
        let non_visual_ids: Vec<&str> = composed_document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "cNvPr")
            .filter_map(|node| node.attribute("id"))
            .collect();
        let unique_non_visual_ids: std::collections::HashSet<&str> =
            non_visual_ids.iter().copied().collect();
        assert_eq!(
            non_visual_ids.len(),
            unique_non_visual_ids.len(),
            "cloned anchors must not reuse cNvPr ids"
        );
        let drawing_rels_path = "xl/drawings/_rels/unicellDrawing1.xml.rels";
        let drawing_rels_xml = std::str::from_utf8(&output[drawing_rels_path]).unwrap();
        let drawing_rels = parse_rels_map(drawing_rels_xml);

        let chart_targets: Vec<String> = drawing_anchor_slices(composed)
            .into_iter()
            .map(|(start, end, _)| &composed[start..end])
            .filter(|anchor| anchor.contains("<c:chart"))
            .map(|anchor| xml_local_element_attr(anchor, "chart", "r:id").unwrap())
            .map(|id| resolve_rel_path(&path_dir(composed_path), &drawing_rels[&id]))
            .collect();
        assert_eq!(chart_targets.len(), 2);
        assert_ne!(chart_targets[0], chart_targets[1]);
        assert!(
            chart_targets
                .iter()
                .any(|target| target == "xl/charts/chart1.xml")
        );
        let cloned_chart = chart_targets
            .iter()
            .find(|target| target.contains("unicellChart"))
            .unwrap();
        assert_eq!(output["xl/charts/chart1.xml"], CHART_XML.as_bytes());
        assert_eq!(
            native_chart_edit::parse_chart_model(
                std::str::from_utf8(&output[cloned_chart]).unwrap()
            )["title"],
            "Clone-only chart title"
        );

        let smartart_anchors: Vec<&str> = drawing_anchor_slices(composed)
            .into_iter()
            .map(|(start, end, _)| &composed[start..end])
            .filter(|anchor| anchor.contains("<dgm:relIds"))
            .collect();
        assert_eq!(smartart_anchors.len(), 2);
        let mut smartart_target_sets = Vec::new();
        for anchor in smartart_anchors {
            let mut targets = Vec::new();
            for attribute in ["r:dm", "r:lo", "r:qs", "r:cs"] {
                let id = drawing_any_element_attr(anchor, "relIds", attribute).unwrap();
                targets.push(resolve_rel_path(
                    &path_dir(composed_path),
                    &drawing_rels[&id],
                ));
            }
            smartart_target_sets.push(targets);
        }
        for index in 0..4 {
            assert_ne!(
                smartart_target_sets[0][index],
                smartart_target_sets[1][index]
            );
        }
        let smartart_cache_targets: Vec<String> = smartart_target_sets
            .iter()
            .filter_map(|targets| {
                let data_xml = std::str::from_utf8(&output[&targets[0]]).unwrap();
                let data_document = roxmltree::Document::parse(data_xml).unwrap();
                let cache_id = data_document
                    .descendants()
                    .find(|node| {
                        node.is_element()
                            && node.tag_name().name() == "dataModelExt"
                            && node.attribute("relId").is_some()
                    })
                    .and_then(|node| node.attribute("relId"))?;
                Some(resolve_rel_path(
                    &path_dir(composed_path),
                    &drawing_rels[cache_id],
                ))
            })
            .collect();
        assert_eq!(smartart_cache_targets, ["xl/diagrams/drawing1.xml"]);
        let cloned_data = smartart_target_sets
            .iter()
            .flatten()
            .find(|target| target.contains("unicellData"))
            .unwrap();
        assert!(
            std::str::from_utf8(&output["xl/diagrams/data1.xml"])
                .unwrap()
                .contains("SmartArt root")
        );
        assert!(
            !std::str::from_utf8(&output["xl/diagrams/data1.xml"])
                .unwrap()
                .contains("Clone-only")
        );
        assert!(
            std::str::from_utf8(&output[cloned_data])
                .unwrap()
                .contains("Clone-only SmartArt root")
        );
        assert!(
            !std::str::from_utf8(&output[cloned_data])
                .unwrap()
                .contains("dataModelExt")
        );
        let cloned_layout = smartart_target_sets
            .iter()
            .flatten()
            .find(|target| target.contains("unicellLayout"))
            .unwrap();
        let cloned_style = smartart_target_sets
            .iter()
            .flatten()
            .find(|target| target.contains("unicellQuickStyle"))
            .unwrap();
        let cloned_colors = smartart_target_sets
            .iter()
            .flatten()
            .find(|target| target.contains("unicellColors"))
            .unwrap();
        assert_eq!(output[cloned_layout], b"layout-byte-exact");
        assert_eq!(output[cloned_style], b"quick-style-byte-exact");
        assert_eq!(output[cloned_colors], b"diagram-colors-byte-exact");

        let relationship_document = roxmltree::Document::parse(drawing_rels_xml).unwrap();
        let relationship_ids: Vec<&str> = relationship_document
            .descendants()
            .filter(|node| node.is_element() && node.has_tag_name("Relationship"))
            .filter_map(|node| node.attribute("Id"))
            .collect();
        let unique_relationship_ids: std::collections::HashSet<&str> =
            relationship_ids.iter().copied().collect();
        assert_eq!(relationship_ids.len(), unique_relationship_ids.len());
        assert!(relationship_ids.iter().all(|id| {
            id.strip_prefix("rId")
                .and_then(|suffix| suffix.parse::<usize>().ok())
                .is_some()
        }));
        for target in drawing_rels.values() {
            let resolved = resolve_rel_path(&path_dir(composed_path), target);
            assert!(
                output.contains_key(&resolved),
                "missing relationship target {resolved}"
            );
        }
        let content_types = std::str::from_utf8(&output["[Content_Types].xml"]).unwrap();
        for target in chart_targets
            .iter()
            .chain(smartart_target_sets.iter().flatten())
            .chain(smartart_cache_targets.iter())
        {
            assert!(
                content_types.contains(&format!("PartName=\"/{target}\"")),
                "missing content type for {target}"
            );
        }

        let second = export_once();
        let first_native: Vec<(&String, &Vec<u8>)> = output
            .iter()
            .filter(|(name, _)| {
                name.contains("unicellChart")
                    || name.contains("unicellData")
                    || name.contains("unicellLayout")
                    || name.contains("unicellQuickStyle")
                    || name.contains("unicellColors")
                    || name.contains("unicellNativeDrawing")
            })
            .collect();
        for (name, bytes) in first_native {
            assert_eq!(
                second.get(name),
                Some(bytes),
                "native clone changed across identical exports: {name}"
            );
        }
    }

    #[test]
    fn unedited_smartart_clone_gets_an_independent_cache_with_recursive_dependencies() {
        let original = native_drawing_fixture_with_cache_dependency();
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let drawing = std::str::from_utf8(&files["xl/drawings/drawing1.xml"]).unwrap();
        let rels = parse_rels_map(
            std::str::from_utf8(&files["xl/drawings/_rels/drawing1.xml.rels"]).unwrap(),
        );
        let mut state = AppState::new();
        let mut objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &state.model,
        );
        let source = objects
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "smartart")
            .unwrap()
            .clone();
        let mut clone = source.clone();
        clone["id"] = json!("unedited-smartart-clone");
        clone["y"] = json!(json_number(&source, "y", 0.0) + 260.0);
        clone["config"]["nativeDrawing"]["clone"] = json!(true);
        clone["config"]["nativeDrawing"]["cloneOfToken"] =
            source["config"]["nativeDrawing"]["token"].clone();
        clone["config"]["nativeDrawing"]["token"] = json!("unedited-smartart-cache-clone");
        objects.push(clone);
        state.objects.insert(0, objects.clone());
        state.source_ooxml = Some(snapshot);

        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
        ]);
        let injected = inject_drawings(generated, &drawing_exports(&objects, false), &[]).unwrap();
        let output = unzip_test_parts(restore_preserved_ooxml(&state, injected).unwrap());
        let drawing_path = "xl/drawings/unicellDrawing1.xml";
        let drawing = std::str::from_utf8(&output[drawing_path]).unwrap();
        let drawing_rels_path = drawing_relationships_path(drawing_path);
        let drawing_rels =
            parse_rels_map(std::str::from_utf8(&output[&drawing_rels_path]).unwrap());
        let mut data_and_cache_targets = Vec::new();
        for anchor in drawing_anchor_slices(drawing)
            .into_iter()
            .map(|(start, end, _)| &drawing[start..end])
            .filter(|anchor| anchor.contains("<dgm:relIds"))
        {
            let data_id = drawing_any_element_attr(anchor, "relIds", "r:dm").unwrap();
            let data_target = resolve_rel_path(&path_dir(drawing_path), &drawing_rels[&data_id]);
            let data_xml = std::str::from_utf8(&output[&data_target]).unwrap();
            let data_document = roxmltree::Document::parse(data_xml).unwrap();
            let cache_id = data_document
                .descendants()
                .find(|node| {
                    node.is_element()
                        && node.tag_name().name() == "dataModelExt"
                        && node.attribute("relId").is_some()
                })
                .and_then(|node| node.attribute("relId"))
                .unwrap();
            let cache_target = resolve_rel_path(&path_dir(drawing_path), &drawing_rels[cache_id]);
            data_and_cache_targets.push((data_target, cache_target));
        }
        assert_eq!(data_and_cache_targets.len(), 2);
        assert_ne!(data_and_cache_targets[0].0, data_and_cache_targets[1].0);
        assert_ne!(data_and_cache_targets[0].1, data_and_cache_targets[1].1);
        let cloned_cache = data_and_cache_targets
            .iter()
            .map(|(_, cache)| cache)
            .find(|cache| cache.contains("unicellNativeDrawing"))
            .unwrap();
        assert_eq!(output[cloned_cache], output["xl/diagrams/drawing1.xml"]);
        let cloned_cache_rels_path = drawing_relationships_path(cloned_cache);
        let cloned_cache_rels =
            parse_rels_map(std::str::from_utf8(&output[&cloned_cache_rels_path]).unwrap());
        let cloned_image =
            resolve_rel_path(&path_dir(cloned_cache), &cloned_cache_rels["rIdCacheImage"]);
        assert_eq!(cloned_image, "xl/media/cache-image.png");
        assert_eq!(output[&cloned_image], b"diagram-cache-image");
        assert!(output.contains_key("xl/diagrams/_rels/drawing1.xml.rels"));
        let content_types = std::str::from_utf8(&output["[Content_Types].xml"]).unwrap();
        assert!(content_types.contains(&format!(
            "PartName=\"/{cloned_cache}\" ContentType=\"application/vnd.ms-office.drawingml.diagramDrawing+xml\""
        )));
    }

    #[test]
    fn native_chart_clone_also_isolates_chart_user_shapes_but_shares_media_leaves() {
        let mut snapshot = OpcPackageSnapshot::default();
        snapshot.parts.insert("[Content_Types].xml".to_string(), br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/charts/chart1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/><Override PartName="/xl/drawings/drawing9.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chartshapes+xml"/></Types>"#.to_vec());
        snapshot.parts.insert(
            "xl/charts/chart1.xml".to_string(),
            CHART_XML.as_bytes().to_vec(),
        );
        snapshot.parts.insert("xl/charts/_rels/chart1.xml.rels".to_string(), br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdUserShapes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartUserShapes" Target="../drawings/drawing9.xml"/></Relationships>"#.to_vec());
        snapshot.parts.insert(
            "xl/drawings/drawing9.xml".to_string(),
            br#"<c:userShapes xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"/>"#
                .to_vec(),
        );
        snapshot.parts.insert("xl/drawings/_rels/drawing9.xml.rels".to_string(), br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImage" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image9.png"/></Relationships>"#.to_vec());
        snapshot
            .parts
            .insert("xl/media/image9.png".to_string(), b"shared-leaf".to_vec());
        let mut parts = std::collections::BTreeMap::new();
        let mut copied = std::collections::HashSet::new();
        let mut cloned_from = std::collections::BTreeMap::new();
        let cloned_chart = clone_native_content_part(
            &snapshot,
            &mut parts,
            "xl/charts/chart1.xml",
            "chart",
            &mut copied,
            &mut cloned_from,
        )
        .unwrap();
        let cloned_chart_rels_path = drawing_relationships_path(&cloned_chart);
        let cloned_chart_rels =
            parse_rels_map(std::str::from_utf8(&parts[&cloned_chart_rels_path]).unwrap());
        let cloned_user_shapes = resolve_rel_path(
            &path_dir(&cloned_chart),
            &cloned_chart_rels["rIdUserShapes"],
        );
        assert_ne!(cloned_user_shapes, "xl/drawings/drawing9.xml");
        assert!(cloned_user_shapes.contains("unicellNativeDrawing"));
        assert!(parts.contains_key(&cloned_user_shapes));
        let cloned_user_rels_path = drawing_relationships_path(&cloned_user_shapes);
        let cloned_user_rels =
            parse_rels_map(std::str::from_utf8(&parts[&cloned_user_rels_path]).unwrap());
        let media = resolve_rel_path(
            &path_dir(&cloned_user_shapes),
            &cloned_user_rels["rIdImage"],
        );
        assert_eq!(media, "xl/media/image9.png");
        assert_eq!(parts[&media], b"shared-leaf");
        assert!(copied.contains(&cloned_chart));
        assert!(copied.contains(&cloned_user_shapes));
        assert_eq!(
            cloned_from.get(&cloned_chart).map(String::as_str),
            Some("xl/charts/chart1.xml")
        );
        assert_eq!(
            cloned_from.get(&cloned_user_shapes).map(String::as_str),
            Some("xl/drawings/drawing9.xml")
        );
        let generated_content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/></Types>"#;
        let merged = merge_content_types(
            generated_content_types,
            &snapshot.parts["[Content_Types].xml"],
            &snapshot,
            &copied,
            &cloned_from,
        )
        .unwrap();
        let merged = std::str::from_utf8(&merged).unwrap();
        assert!(merged.contains(&format!("PartName=\"/{cloned_user_shapes}\" ContentType=\"application/vnd.openxmlformats-officedocument.drawingml.chartshapes+xml\"")));
    }

    #[test]
    fn edited_smartart_removes_cache_relationships_and_unique_media_dependency() {
        let original = native_drawing_fixture_with_cache_dependency();
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let drawing = std::str::from_utf8(&files["xl/drawings/drawing1.xml"]).unwrap();
        let rels = parse_rels_map(
            std::str::from_utf8(&files["xl/drawings/_rels/drawing1.xml.rels"]).unwrap(),
        );
        let mut state = AppState::new();
        let mut objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &state.model,
        );
        let smartart = objects
            .iter_mut()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "smartart")
            .unwrap();
        smartart["config"]["nativeDrawing"]["edits"]["smartart"] =
            json!({"updates":[{"id":"ROOT","text":"Cache invalidated"}]});
        state.objects.insert(0, objects.clone());
        state.source_ooxml = Some(snapshot);

        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
        ]);
        let injected = inject_drawings(generated, &drawing_exports(&objects, false), &[]).unwrap();
        let output = unzip_test_parts(restore_preserved_ooxml(&state, injected).unwrap());
        let data_xml = std::str::from_utf8(&output["xl/diagrams/data1.xml"]).unwrap();
        assert!(data_xml.contains("Cache invalidated"));
        assert!(!data_xml.contains("dataModelExt"));
        let drawing_rels =
            std::str::from_utf8(&output["xl/drawings/_rels/unicellDrawing1.xml.rels"]).unwrap();
        assert!(!drawing_rels.contains("/diagramDrawing"));
        assert!(!output.contains_key("xl/diagrams/drawing1.xml"));
        assert!(!output.contains_key("xl/diagrams/_rels/drawing1.xml.rels"));
        assert!(!output.contains_key("xl/media/cache-image.png"));
        let content_types = std::str::from_utf8(&output["[Content_Types].xml"]).unwrap();
        assert!(!content_types.contains("/xl/diagrams/drawing1.xml"));
    }

    #[test]
    fn deleting_native_smartart_removes_its_anchor_relationships_and_diagram_parts() {
        let original = native_drawing_fixture_with_cache_dependency();
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let drawing = std::str::from_utf8(&files["xl/drawings/drawing1.xml"]).unwrap();
        let rels = parse_rels_map(
            std::str::from_utf8(&files["xl/drawings/_rels/drawing1.xml.rels"]).unwrap(),
        );
        let mut state = AppState::new();
        let mut objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &state.model,
        );
        objects.retain(|object| object["config"]["nativeDrawing"]["kind"] != "smartart");
        state.objects.insert(0, objects.clone());
        state.source_ooxml = Some(snapshot);
        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
        ]);
        let injected = inject_drawings(generated, &drawing_exports(&objects, false), &[]).unwrap();
        let output = unzip_test_parts(restore_preserved_ooxml(&state, injected).unwrap());
        let composed = std::str::from_utf8(&output["xl/drawings/unicellDrawing1.xml"]).unwrap();
        assert_eq!(drawing_anchor_slices(composed).len(), 3);
        assert!(!composed.contains("NativeSmartArt"));
        assert!(!composed.contains("dgm:relIds"));
        assert!(!output.keys().any(|name| name.starts_with("xl/diagrams/")));
        assert!(!output.contains_key("xl/media/cache-image.png"));
        assert!(!output.contains_key("xl/diagrams/_rels/drawing1.xml.rels"));
        let content_types = std::str::from_utf8(&output["[Content_Types].xml"]).unwrap();
        assert!(!content_types.contains("/xl/diagrams/"));
    }

    #[test]
    fn untouched_conditional_formatting_data_validation_extensions_and_dxfs_restore_raw() {
        let original = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main" xmlns:xr="http://schemas.microsoft.com/office/spreadsheetml/2014/revision" mc:Ignorable="x14 xr"><sheetData/><conditionalFormatting sqref="A1:A9"><cfRule type="cellIs" dxfId="0" priority="7" operator="greaterThan" xr:uid="{RAW-CF}"><formula>5</formula><extLst><ext uri="raw-rule-extension"><x14:id>{RULE-GUID}</x14:id></ext></extLst></cfRule><cfRule type="futureMagic" priority="8"><formula>OPAQUE()</formula></cfRule></conditionalFormatting><dataValidations count="1"><dataValidation type="custom" allowBlank="1" showErrorMessage="1" errorTitle="Invalid" error="Keep raw DV" sqref="B2:B8"><formula1>LEN(B2)&gt;2</formula1></dataValidation></dataValidations><extLst><ext uri="{DV-URI}"><x14:dataValidations count="1"><x14:dataValidation type="list"><x14:formula1><xm:f>Sheet2!$A$1:$A$5</xm:f></x14:formula1><xm:sqref>C1:C5</xm:sqref></x14:dataValidation></x14:dataValidations></ext><ext uri="{UNKNOWN-URI}"><x14:futureFeature val="byte-exact"/></ext></extLst></worksheet>"#;
        let mut generated = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/><conditionalFormatting sqref="A1:A9"><cfRule type="cellIs" dxfId="0" priority="1" operator="greaterThan"><formula>5</formula></cfRule></conditionalFormatting></worksheet>"#.to_string();
        let mut state = AppState::new();
        let baseline =
            serde_json::to_string(&state.model.get_conditional_formatting_list(0).unwrap())
                .unwrap();
        state
            .worksheet_features
            .baseline_conditional_formatting
            .insert(0, baseline);
        restore_worksheet_feature_subtrees(&state, 0, &mut generated, original).unwrap();
        assert!(generated.contains("priority=\"7\""));
        assert!(generated.contains("xr:uid=\"{RAW-CF}\""));
        assert!(generated.contains("futureMagic"));
        assert!(generated.contains("Keep raw DV"));
        assert!(generated.contains("x14:dataValidations"));
        assert!(generated.contains("futureFeature val=\"byte-exact\""));
        assert!(generated.contains("mc:Ignorable=\"x14 xr\""));

        let original_styles = br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dxfs count="1"><dxf><font><name val="Aptos Display"/><scheme val="major"/><b/><color theme="5" tint="0.3"/></font><fill><gradientFill type="path"><stop position="0"><color theme="4" tint="0.2"/></stop><stop position="1"><color rgb="FF00AA44"/></stop></gradientFill></fill><protection locked="0" hidden="1"/></dxf></dxfs></styleSheet>"#;
        let generated_styles = br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dxfs count="2"><dxf><font><b/></font></dxf><dxf><fill><patternFill patternType="solid"><fgColor rgb="FFFF0000"/></patternFill></fill></dxf></dxfs></styleSheet>"#;
        let merged_styles =
            String::from_utf8(merge_original_dxfs(generated_styles, original_styles).unwrap())
                .unwrap();
        assert!(merged_styles.contains("Aptos Display"));
        assert!(merged_styles.contains("gradientFill type=\"path\""));
        assert!(merged_styles.contains("protection locked=\"0\" hidden=\"1\""));
        assert!(merged_styles.contains("fgColor rgb=\"FFFF0000\""));
        assert!(merged_styles.contains("count=\"2\""));
    }

    #[test]
    fn edited_conditional_formatting_merges_matching_raw_rule_and_preserves_opaque_and_dv_children()
    {
        let original = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main" xmlns:xr="http://schemas.microsoft.com/office/spreadsheetml/2014/revision"><sheetData/><conditionalFormatting sqref="A1:A9"><cfRule type="cellIs" dxfId="0" priority="7" operator="greaterThan" xr:uid="{KEEP-UID}"><formula>5</formula><extLst><ext uri="raw-child"><x14:id>{KEEP-GUID}</x14:id></ext></extLst></cfRule><cfRule type="futureMagic" priority="8"><formula>OPAQUE()</formula></cfRule><cfRule type="expression" priority="9"><formula>DELETED_RULE()</formula></cfRule></conditionalFormatting><dataValidations count="1"><dataValidation type="whole" operator="between" sqref="D1:D9" promptTitle="Range" prompt="Keep me"><formula1>1</formula1><formula2>10</formula2></dataValidation></dataValidations><extLst><ext uri="{OLD-CF-URI}"><x14:conditionalFormattings><x14:conditionalFormatting><x14:cfRule type="expression" id="{OLD}"><xm:f>OLD_EXT_CF()</xm:f></x14:cfRule><xm:sqref>A1:A9</xm:sqref></x14:conditionalFormatting></x14:conditionalFormattings></ext><ext uri="{DV-URI}"><x14:dataValidations count="1"><x14:dataValidation type="list"><xm:sqref>E1:E9</xm:sqref></x14:dataValidation></x14:dataValidations></ext><ext uri="{UNKNOWN-URI}"><x14:futureFeature val="preserve-me"/></ext></extLst></worksheet>"#;
        let mut generated = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><sheetData/><conditionalFormatting sqref="A1:A9"><cfRule type="cellIs" dxfId="2" priority="1" stopIfTrue="1" operator="greaterThan"><formula>5</formula></cfRule><cfRule type="expression" priority="2"><formula>NEW_RULE()</formula></cfRule></conditionalFormatting><extLst><ext uri="{NEW-CF-URI}"><x14:conditionalFormattings><x14:conditionalFormatting><x14:cfRule type="dataBar" id="{NEW}"/><xm:sqref>A1:A9</xm:sqref></x14:conditionalFormatting></x14:conditionalFormattings></ext></extLst></worksheet>"#.to_string();
        let mut state = AppState::new();
        state
            .worksheet_features
            .baseline_conditional_formatting
            .insert(0, "different baseline".to_string());
        restore_worksheet_feature_subtrees(&state, 0, &mut generated, original).unwrap();
        assert!(generated.contains("xr:uid=\"{KEEP-UID}\""));
        assert!(generated.contains("dxfId=\"2\""));
        assert!(generated.contains("priority=\"1\""));
        assert!(generated.contains("stopIfTrue=\"1\""));
        assert!(generated.contains("raw-child"));
        assert!(generated.contains("futureMagic"));
        assert!(generated.contains("OPAQUE()"));
        assert!(!generated.contains("DELETED_RULE()"));
        assert!(generated.contains("NEW_RULE()"));
        assert!(generated.contains("prompt=\"Keep me\""));
        assert!(!generated.contains("OLD_EXT_CF()"));
        assert!(generated.contains("x14:dataValidations"));
        assert!(generated.contains("futureFeature val=\"preserve-me\""));
        assert!(generated.contains("{NEW-CF-URI}"));
    }

    #[test]
    fn edited_conditional_formatting_keeps_data_bar_x14_guid_paired_and_payload_native() {
        const CF_EXT_URI: &str = "{78C0D931-6437-407d-A8EE-F0AAD7539E65}";
        let original = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main" mc:Ignorable="x14"><sheetData/><conditionalFormatting sqref="A1:A9"><cfRule type="dataBar" priority="7"><dataBar showValue="0"><cfvo type="min"/><cfvo type="max"/><color rgb="FF638EC6"/></dataBar><extLst><ext uri="{B025F937-C7B1-47D3-B67F-A62EFF666E3E}"><x14:id>{ORIGINAL-DATA-BAR}</x14:id></ext></extLst></cfRule></conditionalFormatting><extLst><ext uri="{78C0D931-6437-407d-A8EE-F0AAD7539E65}"><x14:conditionalFormattings><x14:conditionalFormatting><x14:cfRule type="dataBar" id="{ORIGINAL-DATA-BAR}"><x14:dataBar minLength="7" maxLength="93" border="1" gradient="0" axisPosition="middle" direction="rightToLeft"><x14:cfvo type="autoMin"/><x14:cfvo type="autoMax"/><x14:borderColor rgb="FF123456"/><x14:negativeFillColor rgb="FF654321"/><x14:negativeBorderColor rgb="FFABCDEF"/><x14:axisColor rgb="FF010203"/><x14:futureDataBarNode val="preserve-me"/></x14:dataBar></x14:cfRule><xm:sqref>A1:A9</xm:sqref></x14:conditionalFormatting></x14:conditionalFormattings></ext></extLst></worksheet>"#;
        let mut generated = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><sheetData/><conditionalFormatting sqref="B2:B10"><cfRule type="dataBar" priority="1"><dataBar showValue="0"><cfvo type="min"/><cfvo type="max"/><color rgb="FF638EC6"/></dataBar><extLst><ext uri="{B025F937-C7B1-47D3-B67F-A62EFF666E3E}"><x14:id>{00000001-0000-0000-0000-000000000000}</x14:id></ext></extLst></cfRule></conditionalFormatting><conditionalFormatting sqref="C1:C9"><cfRule type="expression" priority="2"><formula>NEW_RULE()</formula></cfRule></conditionalFormatting><extLst><ext uri="{78C0D931-6437-407d-A8EE-F0AAD7539E65}"><x14:conditionalFormattings><x14:conditionalFormatting><x14:cfRule type="dataBar" id="{00000001-0000-0000-0000-000000000000}"><x14:dataBar minLength="0" maxLength="100"><x14:cfvo type="autoMin"/><x14:cfvo type="autoMax"/><x14:negativeFillColor rgb="FFFF0000"/><x14:axisColor rgb="FF000000"/></x14:dataBar></x14:cfRule><xm:sqref>B2:B10</xm:sqref></x14:conditionalFormatting></x14:conditionalFormattings></ext></extLst></worksheet>"#.to_string();
        let mut state = AppState::new();
        state
            .worksheet_features
            .baseline_conditional_formatting
            .insert(0, "different baseline".to_string());

        restore_worksheet_feature_subtrees(&state, 0, &mut generated, original).unwrap();

        let document = roxmltree::Document::parse(&generated).unwrap();
        let main_rule = document
            .root_element()
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "conditionalFormatting")
            .flat_map(|node| node.children())
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "cfRule"
                    && node.attribute("type") == Some("dataBar")
            })
            .unwrap();
        let main_id = main_rule
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "id")
            .and_then(|node| node.text())
            .unwrap();
        let x14_rules: Vec<_> = document
            .root_element()
            .descendants()
            .filter(|node| {
                node.is_element()
                    && node.tag_name().name() == "cfRule"
                    && node.attribute("type") == Some("dataBar")
                    && node
                        .parent()
                        .and_then(|parent| parent.parent())
                        .is_some_and(|parent| parent.tag_name().name() == "conditionalFormattings")
            })
            .collect();
        assert_eq!(x14_rules.len(), 1);
        assert_eq!(main_id, "{ORIGINAL-DATA-BAR}");
        assert_eq!(x14_rules[0].attribute("id"), Some(main_id));
        let x14_data_bar = x14_rules[0]
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "dataBar")
            .unwrap();
        assert_eq!(x14_data_bar.attribute("minLength"), Some("7"));
        assert_eq!(x14_data_bar.attribute("maxLength"), Some("93"));
        assert_eq!(x14_data_bar.attribute("border"), Some("1"));
        assert_eq!(x14_data_bar.attribute("gradient"), Some("0"));
        assert_eq!(x14_data_bar.attribute("axisPosition"), Some("middle"));
        assert_eq!(x14_data_bar.attribute("direction"), Some("rightToLeft"));
        assert!(x14_data_bar.descendants().any(|node| {
            node.is_element()
                && node.tag_name().name() == "futureDataBarNode"
                && node.attribute("val") == Some("preserve-me")
        }));
        let x14_sqref = x14_rules[0]
            .parent()
            .and_then(|node| {
                node.children()
                    .find(|child| child.is_element() && child.tag_name().name() == "sqref")
            })
            .and_then(|node| node.text());
        assert_eq!(x14_sqref, Some("B2:B10"));
        assert!(generated.contains("NEW_RULE()"));
        assert_eq!(generated.matches(CF_EXT_URI).count(), 1);
    }

    #[test]
    fn typed_data_validation_edits_known_fields_without_flattening_opaque_xml() {
        let original = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xr="http://schemas.microsoft.com/office/spreadsheetml/2014/revision"><sheetData/><dataValidations count="2" xWindow="17" yWindow="29"><dataValidation type="whole" operator="between" allowBlank="1" showDropDown="1" showInputMessage="1" showErrorMessage="1" errorStyle="warning" promptTitle="Old" prompt="Keep unknown pieces" sqref="A1:A9" xr:uid="{KEEP-UID}"><formula1>1</formula1><formula2>9</formula2><extLst><ext uri="opaque"><future val="byte-exact"/></ext></extLst></dataValidation><dataValidation type="list" sqref="C1:C4"><formula1>&quot;A,B&quot;</formula1></dataValidation></dataValidations></worksheet>"#;
        let mut transport = parse_data_validation_sheet(0, original);
        assert_eq!(transport.rules.len(), 2);
        assert!(!transport.rules[0].in_cell_dropdown);
        assert_eq!(transport.rules[1].formula1.as_deref(), Some("\"A,B\""));
        transport.rules[0].sqref = "B2:B20 D2:D20".to_string();
        transport.rules[0].validation_type = "decimal".to_string();
        transport.rules[0].operator = Some("greaterThanOrEqual".to_string());
        transport.rules[0].formula1 = Some("2.5".to_string());
        transport.rules[0].formula2 = None;
        transport.rules[0].in_cell_dropdown = true;
        transport.rules[0].prompt_title = Some("New & safe".to_string());
        transport.rules.remove(1);

        let mut generated = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xr="http://schemas.microsoft.com/office/spreadsheetml/2014/revision"><sheetData/><dataValidations count="1"><dataValidation type="custom" sqref="Z1"><formula1>FALSE</formula1></dataValidation></dataValidations></worksheet>"#.to_string();
        apply_data_validation_sheet(&mut generated, &transport).unwrap();
        assert!(generated.contains("count=\"1\""));
        assert!(generated.contains("xWindow=\"17\""));
        assert!(generated.contains("yWindow=\"29\""));
        assert!(generated.contains("type=\"decimal\""));
        assert!(generated.contains("operator=\"greaterThanOrEqual\""));
        assert!(generated.contains("sqref=\"B2:B20 D2:D20\""));
        assert!(generated.contains("showDropDown=\"0\""));
        assert!(generated.contains("promptTitle=\"New &amp; safe\""));
        assert!(generated.contains("xr:uid=\"{KEEP-UID}\""));
        assert!(generated.contains("future val=\"byte-exact\""));
        assert!(generated.contains("<formula1>2.5</formula1>"));
        assert!(!generated.contains("<formula2>"));
        assert!(!generated.contains("C1:C4"));
        roxmltree::Document::parse(&generated).unwrap();
    }

    #[test]
    fn typed_data_validation_edits_prefixed_nodes_without_duplicate_children() {
        let original = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:u="urn:unicell:opaque"><sheetData/><x:dataValidations count="2" u:window="keep"><x:dataValidation type="whole" operator="between" sqref="A1:A9" u:uid="keep"><x:formula1 u:metadata="keep">1</x:formula1><x:formula2>9</x:formula2><x:extLst><u:payload value="byte-exact"/></x:extLst></x:dataValidation><x:dataValidation type="list" sqref="B1:B4"><x:extLst><u:formula1>OPAQUE_EXTENSION_FORMULA</u:formula1></x:extLst></x:dataValidation></x:dataValidations></worksheet>"#;
        let mut transport = parse_data_validation_sheet(0, original);
        assert_eq!(transport.rules.len(), 2);
        assert_eq!(transport.rules[0].formula1.as_deref(), Some("1"));
        assert_eq!(transport.rules[1].formula1, None);

        transport.rules[0].formula1 = Some("2 & 3".to_string());
        transport.rules[0].formula2 = None;
        transport.rules[1].formula1 = Some("\"Yes,No\"".to_string());
        let mut added = transport.rules[1].clone();
        added.id = "added-prefixed-rule".to_string();
        added.sqref = "C1:C4".to_string();
        added.raw_xml = None;
        transport.rules.push(added);

        let mut generated = original.to_string();
        apply_data_validation_sheet(&mut generated, &transport).unwrap();
        let document = roxmltree::Document::parse(&generated).unwrap();
        let container = document
            .root_element()
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "dataValidations")
            .unwrap();
        let rules = container
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "dataValidation")
            .collect::<Vec<_>>();
        assert_eq!(rules.len(), 3);
        assert_eq!(
            direct_child_text(rules[0], "formula1").as_deref(),
            Some("2 & 3")
        );
        assert!(
            rules[0]
                .children()
                .all(|node| !node.is_element() || node.tag_name().name() != "formula2")
        );
        assert_eq!(
            rules[1]
                .children()
                .filter(|node| node.is_element() && node.tag_name().name() == "formula1")
                .count(),
            1
        );
        assert_eq!(
            direct_child_text(rules[1], "formula1").as_deref(),
            Some("\"Yes,No\"")
        );
        assert_eq!(
            direct_child_text(rules[2], "formula1").as_deref(),
            Some("\"Yes,No\"")
        );
        assert_eq!(generated.matches("<x:dataValidation ").count(), 3);
        assert_eq!(generated.matches("<x:formula1").count(), 3);
        assert!(!generated.contains("<dataValidation "));
        assert!(!generated.contains("<formula1>"));
        assert!(generated.contains("u:metadata=\"keep\""));
        assert!(generated.contains("u:window=\"keep\""));
        assert!(generated.contains("<u:payload value=\"byte-exact\"/>"));
        assert!(generated.contains("<u:formula1>OPAQUE_EXTENSION_FORMULA</u:formula1>"));
        assert!(generated.contains("</x:dataValidations>"));
    }

    #[test]
    fn data_validation_ranges_and_formulas_follow_row_and_column_structure_edits() {
        assert_eq!(
            transform_data_validation_sqref("A1:A10 C5:C6", true, 3, 2, false),
            "A1:A12 C7:C8"
        );
        assert_eq!(
            transform_data_validation_sqref("A1:A10 B2:B4", true, 2, 4, true),
            "A1:A6"
        );
        assert_eq!(
            transform_data_validation_sqref("$B$2:$D$8 F:F", false, 3, 1, false),
            "$B$2:$E$8 G:G"
        );
        assert_eq!(
            transform_data_validation_formula(
                "Sheet2!$A$1:$A$5",
                true,
                3,
                2,
                false,
                "Sheet2",
                false,
            ),
            "Sheet2!$A$1:$A$7"
        );
        assert_eq!(
            transform_data_validation_formula(
                "Other!$A$1:$A$5",
                true,
                3,
                2,
                false,
                "Sheet2",
                false,
            ),
            "Other!$A$1:$A$5"
        );
        assert_eq!(
            transform_data_validation_formula("AND(A2>0,A2<10)", true, 2, 1, false, "Sheet1", true,),
            "AND(A3>0,A3<10)"
        );
        assert_eq!(
            transform_data_validation_formula("\"A1,A2\"", true, 1, 4, false, "Sheet1", true,),
            "\"A1,A2\""
        );
    }

    #[test]
    fn data_validation_sidecars_follow_sheet_rename_duplicate_move_and_delete() {
        let mut state = AppState::new();
        state.model.new_sheet().unwrap();
        let xml = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/><dataValidations count="1"><dataValidation type="list" sqref="A1:A3"><formula1>Sheet2!$A$1:$A$3</formula1></dataValidation></dataValidations></worksheet>"#;
        state
            .worksheet_features
            .data_validations
            .insert(0, parse_data_validation_sheet(0, xml));

        api_sheet(
            &mut state,
            &serde_json::to_vec(&json!({"op":"rename","sheet":1,"name":"源 数据"})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            state.worksheet_features.data_validations[&0].rules[0]
                .formula1
                .as_deref(),
            Some("'源 数据'!$A$1:$A$3")
        );

        api_sheet(
            &mut state,
            &serde_json::to_vec(&json!({"op":"duplicate","sheet":0})).unwrap(),
        )
        .unwrap();
        assert!(state.worksheet_features.data_validations.contains_key(&0));
        assert!(state.worksheet_features.data_validations.contains_key(&1));
        assert_ne!(
            state.worksheet_features.data_validations[&0].rules[0].id,
            state.worksheet_features.data_validations[&1].rules[0].id
        );

        api_sheet(
            &mut state,
            &serde_json::to_vec(&json!({"op":"move","sheet":0,"to":2})).unwrap(),
        )
        .unwrap();
        assert!(state.worksheet_features.data_validations.contains_key(&0));
        assert!(state.worksheet_features.data_validations.contains_key(&2));

        api_sheet(
            &mut state,
            &serde_json::to_vec(&json!({"op":"delete","sheet":2})).unwrap(),
        )
        .unwrap();
        assert_eq!(state.worksheet_features.data_validations.len(), 1);
        assert!(state.worksheet_features.data_validations.contains_key(&0));
        assert!(state.worksheet_features.data_validation_dirty.contains(&0));
    }

    #[test]
    fn edited_standard_data_validation_is_injected_after_opaque_ooxml_restore() {
        let original_sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><sheetData/><dataValidations count="1"><dataValidation type="whole" operator="between" sqref="A1:A9"><formula1>1</formula1><formula2>9</formula2></dataValidation></dataValidations><extLst><ext uri="{DV-EXT}"><x14:dataValidations count="1"><x14:dataValidation type="list"><x14:formula1><xm:f>Sheet2!$A$1:$A$5</xm:f></x14:formula1><xm:sqref>C1:C5</xm:sqref></x14:dataValidation></x14:dataValidations></ext></extLst></worksheet>"#;
        let original = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            ("xl/workbook.xml", br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="sheetRel"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheetRel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
            ("xl/worksheets/sheet1.xml", original_sheet.as_bytes()),
        ]);
        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            ("xl/workbook.xml", br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="sheetRel"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheetRel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#),
        ]);
        let mut state = AppState::new();
        state.source_ooxml = Some(snapshot_opc_package(&original).unwrap());
        let mut transport = parse_data_validation_sheet(0, original_sheet);
        transport.rules[0].sqref = "B2:B30".to_string();
        transport.rules[0].formula2 = Some("99".to_string());
        state
            .worksheet_features
            .data_validations
            .insert(0, transport);
        state.worksheet_features.data_validation_dirty.insert(0);

        let restored = restore_preserved_ooxml(&state, generated).unwrap();
        let output = unzip_test_parts(apply_typed_data_validations(&state, restored).unwrap());
        let worksheet = String::from_utf8(output["xl/worksheets/sheet1.xml"].clone()).unwrap();
        assert!(worksheet.contains("sqref=\"B2:B30\""));
        assert!(worksheet.contains("<formula2>99</formula2>"));
        assert!(!worksheet.contains("sqref=\"A1:A9\""));
        assert!(worksheet.contains("x14:dataValidations"));
        assert!(worksheet.contains("Sheet2!$A$1:$A$5"));
        roxmltree::Document::parse(&worksheet).unwrap();
    }

    #[test]
    fn html_capture_is_high_dpi_bounded_and_stays_in_an_opaque_sandbox() {
        assert_eq!(
            html_capture_geometry(&json!({ "width": 300, "height": 120, "scale": 3 })).unwrap(),
            (300, 120, 3)
        );
        // 4096² at 4× would be excessive; the pixel budget reduces it to a safe 1× render.
        assert_eq!(
            html_capture_geometry(&json!({ "width": 4096, "height": 4096, "scale": 4 })).unwrap(),
            (4096, 4096, 1)
        );
        let document = html_capture_document("<b title=\"x\">沙盒</b>");
        assert!(
            document.contains("sandbox=\"allow-scripts allow-forms allow-modals allow-popups\"")
        );
        assert!(!document.contains("allow-same-origin"));
        assert!(chromium_capture_error_is_retryable(
            "Chromium capture exited with exit code: 0x80000003"
        ));
        assert!(chromium_capture_error_is_retryable(
            "read Chromium capture: file temporarily unavailable"
        ));
        assert!(!chromium_capture_error_is_retryable(
            "Chromium HTML capture timed out after 12 seconds"
        ));
        assert!(!chromium_capture_error_is_retryable(
            "HTML capture area is too large"
        ));
        assert!(document.contains("&lt;b title=&quot;x&quot;&gt;沙盒&lt;/b&gt;"));
    }

    #[test]
    fn unsupported_opc_parts_relationships_and_parent_references_round_trip() {
        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            ("xl/workbook.xml", br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>edited</t></is></c></row></sheetData></worksheet>"#),
            ("xl/worksheets/_rels/sheet1.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com" TargetMode="External"/></Relationships>"#),
        ]);
        let original = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="bin" ContentType="application/vnd.ms-office.vbaProject"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.ms-excel.sheet.macroEnabled.main+xml"/><Override PartName="/xl/connections.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.connections+xml"/><Override PartName="/xl/externalLinks/externalLink1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml"/><Override PartName="/xl/pivotCache/pivotCacheDefinition1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml"/><Override PartName="/xl/pivotTables/pivotTable1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml"/></Types>"#),
            ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/><Relationship Id="rIdCustom" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml" Target="customXml/item1.xml"/><Relationship Id="rIdSig" Type="http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/origin" Target="_xmlsignatures/origin.sigs"/></Relationships>"#),
            ("xl/workbook.xml", br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rIdSheet"/></sheets><externalReferences><externalReference r:id="rId1"/></externalReferences><pivotCaches><pivotCache cacheId="1" r:id="rId2"/></pivotCaches></workbook>"#),
            ("xl/_rels/workbook.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdSheet" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink" Target="externalLinks/externalLink1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="pivotCache/pivotCacheDefinition1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/connections" Target="connections.xml"/><Relationship Id="rId4" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/></Relationships>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><pivotTableParts count="1"><pivotTablePart r:id="rId1"/></pivotTableParts></worksheet>"#),
            ("xl/worksheets/_rels/sheet1.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../pivotTables/pivotTable1.xml"/></Relationships>"#),
            ("xl/connections.xml", b"connections-byte-exact"),
            ("xl/externalLinks/externalLink1.xml", b"external-link-byte-exact"),
            ("xl/externalLinks/_rels/externalLink1.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="https://example.com/source.xlsx" TargetMode="External"/></Relationships>"#),
            ("xl/pivotCache/pivotCacheDefinition1.xml", b"pivot-cache-byte-exact"),
            ("xl/pivotTables/pivotTable1.xml", b"pivot-table-byte-exact"),
            ("xl/vbaProject.bin", b"vba-byte-exact"),
            ("customXml/item1.xml", b"custom-xml-byte-exact"),
            ("_xmlsignatures/origin.sigs", b"signature-must-not-survive-edit"),
        ]);
        let snapshot = snapshot_opc_package(&original).unwrap();
        assert!(snapshot.macro_enabled);
        let mut state = AppState::new();
        state.source_ooxml = Some(snapshot);
        state.excel_extension = "xlsm".to_string();
        let output = unzip_test_parts(restore_preserved_ooxml(&state, generated).unwrap());

        assert_eq!(output["xl/vbaProject.bin"], b"vba-byte-exact");
        assert_eq!(output["xl/connections.xml"], b"connections-byte-exact");
        assert_eq!(
            output["xl/pivotCache/pivotCacheDefinition1.xml"],
            b"pivot-cache-byte-exact"
        );
        assert_eq!(output["customXml/item1.xml"], b"custom-xml-byte-exact");
        assert!(!output.contains_key("_xmlsignatures/origin.sigs"));

        let content_types = String::from_utf8(output["[Content_Types].xml"].clone()).unwrap();
        assert!(content_types.contains("sheet.macroEnabled.main+xml"));
        assert!(content_types.contains("/xl/connections.xml"));
        assert!(content_types.contains("Extension=\"bin\""));

        let workbook_rels =
            String::from_utf8(output["xl/_rels/workbook.xml.rels"].clone()).unwrap();
        let workbook_rels_doc = roxmltree::Document::parse(&workbook_rels).unwrap();
        let external_id = workbook_rels_doc
            .descendants()
            .find(|n| {
                n.is_element() && n.attribute("Target") == Some("externalLinks/externalLink1.xml")
            })
            .unwrap()
            .attribute("Id")
            .unwrap()
            .to_string();
        let workbook = String::from_utf8(output["xl/workbook.xml"].clone()).unwrap();
        assert!(workbook.contains(&format!("<externalReference r:id=\"{external_id}\"")));
        assert!(workbook.contains("<pivotCaches>"));

        let sheet_rels =
            String::from_utf8(output["xl/worksheets/_rels/sheet1.xml.rels"].clone()).unwrap();
        let sheet_rels_doc = roxmltree::Document::parse(&sheet_rels).unwrap();
        let pivot_id = sheet_rels_doc
            .descendants()
            .find(|n| {
                n.is_element() && n.attribute("Target") == Some("../pivotTables/pivotTable1.xml")
            })
            .unwrap()
            .attribute("Id")
            .unwrap()
            .to_string();
        let sheet = String::from_utf8(output["xl/worksheets/sheet1.xml"].clone()).unwrap();
        assert!(sheet.contains(&format!("<pivotTablePart r:id=\"{pivot_id}\"")));
        assert!(sheet_rels.contains("https://example.com"));

        let package_rels = String::from_utf8(output["_rels/.rels"].clone()).unwrap();
        assert!(package_rels.contains("customXml/item1.xml"));
        assert!(!package_rels.contains("digital-signature"));
    }

    #[test]
    fn pivot_cache_refresh_api_patches_native_part_after_restore_without_flattening() {
        let original = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/pivotCache/cache-native.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml"/></Types>"#),
            ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            ("xl/workbook.xml", br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="sheetRel"/></sheets><pivotCaches><pivotCache cacheId="7" r:id="cacheRel"/></pivotCaches></workbook>"#),
            ("xl/_rels/workbook.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheetRel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="cacheRel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="pivotCache/cache-native.xml"/></Relationships>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#),
            ("xl/pivotCache/cache-native.xml", br#"<?xml version="1.0"?><pivotCacheDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" refreshOnLoad="0" enableRefresh="1" saveData="1" vendor="keep"><cacheSource type="worksheet"><worksheetSource sheet="Sheet1" ref="A1:C20"/></cacheSource><cacheFields count="3"><cacheField name="A"/><cacheField name="B"/><cacheField name="C"/></cacheFields><extLst><ext uri="opaque"><future keep="byte-exact"/></ext></extLst></pivotCacheDefinition>"#),
        ]);
        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            ("xl/workbook.xml", br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="sheetRel"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheetRel" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#),
        ]);
        let mut state = AppState::new();
        state.source_ooxml = Some(snapshot_opc_package(&original).unwrap());
        api_pivot_caches(
            &mut state,
            &serde_json::to_vec(&json!({
                "op":"update", "cacheId":7, "part":"xl/pivotCache/cache-native.xml",
                "patch":{"refreshOnLoad":true,"backgroundQuery":false,"saveData":null,"missingItemsLimit":250}
            }))
            .unwrap(),
        )
        .unwrap();
        let model = pivot_cache_model_with_edits(&state).unwrap();
        assert_eq!(model["caches"][0]["refresh"]["refreshOnLoad"], true);
        assert_eq!(model["caches"][0]["refresh"]["backgroundQuery"], false);
        assert_eq!(model["caches"][0]["refresh"]["saveData"], Value::Null);
        assert_eq!(model["caches"][0]["refresh"]["missingItemsLimit"], 250);

        let output = unzip_test_parts(restore_preserved_ooxml(&state, generated).unwrap());
        let cache = String::from_utf8(output["xl/pivotCache/cache-native.xml"].clone()).unwrap();
        assert!(cache.contains("refreshOnLoad=\"1\""));
        assert!(cache.contains("backgroundQuery=\"0\""));
        assert!(cache.contains("missingItemsLimit=\"250\""));
        assert!(!cache.contains("saveData="));
        assert!(cache.contains("vendor=\"keep\""));
        assert!(cache.contains("<future keep=\"byte-exact\"/>"));
        assert!(cache.contains("<cacheField name=\"A\"/>"));
        roxmltree::Document::parse(&cache).unwrap();

        api_pivot_caches(
            &mut state,
            &serde_json::to_vec(&json!({
                "op":"reset", "part":"xl/pivotCache/cache-native.xml"
            }))
            .unwrap(),
        )
        .unwrap();
        let reset = pivot_cache_model_with_edits(&state).unwrap();
        assert_eq!(reset["caches"][0]["refresh"]["refreshOnLoad"], false);
        assert_eq!(reset["caches"][0]["refresh"]["enableRefresh"], true);
        assert_eq!(reset["caches"][0]["refresh"]["saveData"], true);
        assert_eq!(
            reset["caches"][0]["refresh"]["backgroundQuery"],
            Value::Null
        );
        assert_eq!(
            reset["caches"][0]["refresh"]["missingItemsLimit"],
            Value::Null
        );
        assert_ne!(reset["caches"][0]["edited"], true);
    }

    #[test]
    fn custom_data_validation_evaluates_candidate_in_a_sandbox_without_mutating_cell() {
        let mut state = AppState::new();
        state.model.set_user_input(0, 3, 1, "4").unwrap();
        state
            .worksheet_features
            .data_validations
            .entry(0)
            .or_default()
            .rules
            .push(DataValidationRule {
                id: "dv-custom".to_string(),
                sqref: "A1:A5".to_string(),
                validation_type: "custom".to_string(),
                operator: None,
                allow_blank: false,
                in_cell_dropdown: true,
                show_input_message: false,
                show_error_message: true,
                error_style: Some("stop".to_string()),
                ime_mode: None,
                prompt_title: None,
                prompt: None,
                error_title: Some("Invalid".to_string()),
                error: Some("Must be less than 10".to_string()),
                formula1: Some("=A1<10".to_string()),
                formula2: None,
                raw_xml: None,
            });
        let accepted = validate_data_validation_candidate(
            &state,
            0,
            &json!({"row":3,"col":1,"id":"dv-custom","value":"8"}),
        )
        .unwrap();
        assert_eq!(accepted["valid"], true);
        let rejected = validate_data_validation_candidate(
            &state,
            0,
            &json!({"row":3,"col":1,"id":"dv-custom","value":"12"}),
        )
        .unwrap();
        assert_eq!(rejected["valid"], false);
        assert_eq!(rejected["reason"], "customFormulaFalse");
        assert_eq!(state.model.get_cell_content(0, 3, 1).unwrap(), "4");
        assert_eq!(state.model.undo_depth(), 1);
    }

    #[test]
    fn local_pivot_refresh_reads_a_live_source_range_writes_results_and_undoes_atomically() {
        let mut state = AppState::new();
        for (row, region, amount) in [(2, "East", "10"), (3, "West", "20"), (4, "East", "5")] {
            state.model.set_user_input(0, row, 1, region).unwrap();
            state.model.set_user_input(0, row, 2, amount).unwrap();
        }
        state.model.set_user_input(0, 1, 1, "Region").unwrap();
        state.model.set_user_input(0, 1, 2, "Amount").unwrap();
        state.clear_application_history();
        handle_api_with_history(
            &mut state,
            "/api/pivot-local-refresh",
            "",
            &serde_json::to_vec(&json!({
                "op":"apply",
                "sourceRange":{"sheet":0,"r0":1,"c0":1,"r1":4,"c1":2},
                "rows":["Region"],
                "columns":[],
                "values":[{"field":"Amount","aggregate":"sum","caption":"Total"}],
                "grandTotals":{"rows":true,"columns":false},
                "output":{"sheet":0,"row":6,"col":1}
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(state.app_undo.len(), 1);
        assert!(!state.model.get_cell_content(0, 6, 1).unwrap().is_empty());
        let rendered = (6..=12)
            .flat_map(|row| (1..=4).map(move |col| (row, col)))
            .map(|(row, col)| state.model.get_formatted_cell_value(0, row, col).unwrap())
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|value| value == "East"));
        assert!(rendered.iter().any(|value| value == "15"));
        assert!(rendered.iter().any(|value| value == "West"));
        assert!(rendered.iter().any(|value| value == "20"));
        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 6, 1).unwrap(), "");
    }

    #[test]
    fn native_data_connection_edits_and_safe_m_runtime_are_transactional() {
        let mut state = AppState::new();
        state.source_ooxml = Some(OpcPackageSnapshot {
            parts: std::collections::BTreeMap::from([
                (
                    "[Content_Types].xml".to_string(),
                    br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/connections.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.connections+xml"/></Types>"#.to_vec(),
                ),
                (
                    "xl/connections.xml".to_string(),
                    br#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:v="urn:vendor" v:keep="root"><connection id="7" name="Orders" refreshOnLoad="0"><dbPr connection="Server=local;User=alice;Password=secret" command="select old" v:keep="db"/></connection></connections>"#.to_vec(),
                ),
            ]),
            macro_enabled: false,
        });
        state.clear_application_history();
        handle_api_with_history(
            &mut state,
            "/api/native-data",
            "",
            &serde_json::to_vec(&json!({
                "op":"update","patch":{"connectionEdits":[{
                    "part":"xl/connections.xml","id":7,
                    "attributes":{"refreshOnLoad":true},"commandText":"select new"
                }]}
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(state.app_undo.len(), 1);
        let inspected = native_data_model_with_edits(&state).unwrap();
        assert_eq!(
            inspected["connections"][0]["attributes"]["refreshOnLoad"],
            "1"
        );
        assert_eq!(inspected["connections"][0]["sourceRedacted"], true);
        assert_eq!(
            inspected["connections"][0]["source"]["connection"],
            "Server=local;User=alice;Password=***"
        );
        let parts = materialize_native_data_parts(&state).unwrap();
        let connection = String::from_utf8(parts["xl/connections.xml"].clone()).unwrap();
        assert!(connection.contains("command=\"select new\""));
        assert!(connection.contains("Password=secret"));
        assert!(connection.contains("v:keep=\"db\""));
        assert!(state.undo_application().unwrap());
        assert!(state.native_data_edits.is_empty());
        let reset = String::from_utf8(
            materialize_native_data_parts(&state).unwrap()["xl/connections.xml"].clone(),
        )
        .unwrap();
        assert!(reset.contains("command=\"select old\""));

        let runtime = native_data_runtime::execute_m_subset(&json!({
            "m":"Table.SelectRows(Input, each [Amount] >= 3)",
            "inputs":{"Input":{"columns":["Name","Amount"],"rows":[["a",2],["b",4]]}}
        }))
        .unwrap();
        assert_eq!(runtime["ok"], true);
        assert_eq!(runtime["rows"], json!([["b", 4]]));
    }

    #[test]
    fn drawing_injection_appends_to_existing_sheet_relationships_with_a_free_id() {
        let generated = test_zip(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("xl/worksheets/sheet1.xml", br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/></worksheet>"#),
            ("xl/worksheets/_rels/sheet1.xml.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdDrawing1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com" TargetMode="External"/></Relationships>"#),
        ]);
        let object = json!({
            "id":"image-test", "type":"image", "mode":"cell", "sheet":0,
            "r":1, "c":1, "x":0, "y":0, "w":1, "h":1,
            "png":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
        });
        let output = unzip_test_parts(inject_drawings(generated, &[object], &[]).unwrap());
        let rels =
            String::from_utf8(output["xl/worksheets/_rels/sheet1.xml.rels"].clone()).unwrap();
        assert!(rels.contains("https://example.com"));
        assert!(rels.contains("Id=\"rIdDrawing2\""));
        assert!(rels.contains("Target=\"../drawings/unicellDrawing1.xml\""));
        let sheet = String::from_utf8(output["xl/worksheets/sheet1.xml"].clone()).unwrap();
        assert!(sheet.contains("<drawing r:id=\"rIdDrawing2\"/>"));
    }

    #[test]
    fn unchanged_shared_and_dynamic_array_formula_transport_is_restored_but_edits_win() {
        let mut authoring = AppState::new();
        authoring.model.set_user_input(0, 1, 1, "1").unwrap();
        authoring.model.set_user_input(0, 2, 1, "2").unwrap();
        authoring.model.set_user_input(0, 1, 2, "=A1+1").unwrap();
        authoring.model.set_user_input(0, 2, 2, "=A2+1").unwrap();
        authoring
            .model
            .set_user_input(0, 1, 4, "=SEQUENCE(2)")
            .unwrap();
        authoring.model.evaluate();
        let mut original_parts = unzip_test_parts(model_to_xlsx_bytes(&authoring).unwrap());
        let mut sheet =
            String::from_utf8(original_parts["xl/worksheets/sheet1.xml"].clone()).unwrap();
        replace_test_cell_formula(
            &mut sheet,
            "B1",
            r#"<f t="shared" ref="B1:B2" si="7">A1+1</f>"#,
            None,
        );
        replace_test_cell_formula(&mut sheet, "B2", r#"<f t="shared" si="7"/>"#, None);
        replace_test_cell_formula(
            &mut sheet,
            "D1",
            r#"<f t="array" ref="D1:D2" aca="1" ca="1">_xlfn.SEQUENCE(2)</f>"#,
            Some("3"),
        );
        replace_test_cell_formula(&mut sheet, "D2", r#"<f ca="1"/>"#, None);
        original_parts.insert("xl/worksheets/sheet1.xml".to_string(), sheet.into_bytes());
        let original = test_zip_from_map(original_parts);

        let mut imported = AppState::new();
        load_xlsx_into_state(&mut imported, &original).unwrap();
        assert_eq!(
            imported.formula_transport.sheets["xl/worksheets/sheet1.xml"]
                .groups
                .len(),
            2
        );
        let preserved = unzip_test_parts(model_to_preserved_xlsx_bytes(&imported).unwrap());
        let preserved_sheet =
            String::from_utf8(preserved["xl/worksheets/sheet1.xml"].clone()).unwrap();
        assert!(preserved_sheet.contains(r#"<f t="shared" ref="B1:B2" si="7">A1+1</f>"#));
        assert!(preserved_sheet.contains(r#"<f t="shared" si="7"/>"#));
        assert!(
            preserved_sheet.contains(r#"r="D1" cm="3""#)
                || preserved_sheet.contains(r#"cm="3" r="D1""#)
        );
        assert!(preserved_sheet.contains(r#"aca="1" ca="1""#));
        assert!(preserved_sheet.contains(r#"<f ca="1"/>"#));

        imported.model.set_user_input(0, 2, 2, "=A2+9").unwrap();
        imported.model.evaluate();
        let edited = unzip_test_parts(model_to_preserved_xlsx_bytes(&imported).unwrap());
        let edited_sheet = String::from_utf8(edited["xl/worksheets/sheet1.xml"].clone()).unwrap();
        assert!(!edited_sheet.contains(r#"t="shared""#));
        assert!(edited_sheet.contains("A2+9"));
        // The independent dynamic-array group was untouched and remains transport-faithful.
        assert!(edited_sheet.contains(r#"cm="3""#));
    }

    #[test]
    fn len_trim_and_unicode_broadcast_over_ranges_without_nimpl() {
        let mut state = AppState::new();
        state.model.set_user_input(0, 1, 1, "  ab  c  ").unwrap();
        state.model.set_user_input(0, 2, 1, "猫").unwrap();
        state.model.set_user_input(0, 1, 2, "=LEN(A1:A2)").unwrap();
        state.model.set_user_input(0, 1, 3, "=TRIM(A1:A2)").unwrap();
        state
            .model
            .set_user_input(0, 1, 4, "=UNICODE(A1:A2)")
            .unwrap();
        state.model.evaluate();
        assert_eq!(state.model.get_formatted_cell_value(0, 1, 2).unwrap(), "9");
        assert_eq!(state.model.get_formatted_cell_value(0, 2, 2).unwrap(), "1");
        assert_eq!(
            state.model.get_formatted_cell_value(0, 1, 3).unwrap(),
            "ab c"
        );
        assert_eq!(state.model.get_formatted_cell_value(0, 2, 3).unwrap(), "猫");
        assert_eq!(state.model.get_formatted_cell_value(0, 1, 4).unwrap(), "32");
        assert_eq!(
            state.model.get_formatted_cell_value(0, 2, 4).unwrap(),
            "29483"
        );
    }

    #[test]
    fn native_print_uses_excel_area_margins_breaks_titles_and_headers() {
        let mut state = AppState::new();
        // Print titles intentionally sit outside Print_Area; Excel still prepends
        // them to every physical page.
        state.model.set_user_input(0, 1, 2, "Title").unwrap();
        state.model.set_user_input(0, 12, 6, "Last").unwrap();
        let request = serde_json::to_vec(&json!({
            "op":"update",
            "patch":{"worksheets":[{
                "sheet":"Sheet1",
                "pageMargins":{"left":0.25,"right":0.5,"top":0.75,"bottom":1.0,"header":0.2,"footer":0.3},
                "pageSetup":{"paperSize":11,"orientation":"landscape","fitToWidth":1,"fitToHeight":0},
                "printOptions":{"gridLines":true},
                "headerFooter":{"oddHeader":"&L&F&C&A&R&P/&N","oddFooter":"&CPage &P"},
                "rowBreaks":{"items":[{"id":5,"man":1,"min":0,"max":16383}]},
                "colBreaks":{"items":[{"id":3,"man":1,"min":0,"max":1048575}]},
                "printArea":"'Sheet1'!$B$2:$F$12",
                "printTitles":"'Sheet1'!$1:$1,'Sheet1'!$B:$B"
            }]}
        })).unwrap();
        api_page_review(&mut state, &request).unwrap();
        let response = api_print_html_paged(
            &state,
            "sheet=0&paper=sheet&orientation=sheet&scaling=none&autoprint=0",
        )
        .unwrap();
        let mut html = String::new();
        response.into_reader().read_to_string(&mut html).unwrap();
        assert!(html.contains("data-print-paper=\"A5\""));
        assert!(html.contains("data-print-orientation=\"landscape\""));
        assert!(html.contains("data-print-row-start=\"2\""));
        assert!(html.contains("data-print-col-start=\"2\""));
        assert!(html.contains("data-print-rows=\"11\""));
        assert!(html.contains("class=\"print-gridlines\""));
        assert!(html.contains("page-header"));
        assert!(html.contains("Sheet1"));
        assert!(html.contains("Page 1"));
        assert!(html.contains("left:24.000px"));
        // Print-title row/column appear on every explicit physical page.
        let pages = html.matches("<section class=\"print-page").count();
        assert!(pages >= 2);
        assert!(html.matches("Title").count() >= pages);

        state.model.new_sheet().unwrap();
        state.model.set_user_input(1, 1, 1, "Second sheet").unwrap();
        let second_sheet_setup = serde_json::to_vec(&json!({
            "op":"update",
            "patch":{"worksheets":[{
                "sheet":"Sheet2",
                "pageMargins":{"left":0.4,"right":0.4,"top":0.5,"bottom":0.5},
                "pageSetup":{"paperSize":8,"orientation":"portrait"}
            }]}
        }))
        .unwrap();
        api_page_review(&mut state, &second_sheet_setup).unwrap();
        let response = api_print_html_paged(
            &state,
            "scope=workbook&paper=sheet&orientation=sheet&scaling=none&autoprint=0",
        )
        .unwrap();
        let mut workbook_html = String::new();
        response
            .into_reader()
            .read_to_string(&mut workbook_html)
            .unwrap();
        assert!(workbook_html.contains("data-print-scope=\"workbook\""));
        assert!(workbook_html.contains("Second sheet"));
        assert!(workbook_html.matches("<section class=\"print-page").count() > pages);
        assert!(workbook_html.contains("page:unicell-sheet-0"));
        assert!(workbook_html.contains("page:unicell-sheet-1"));
        assert!(workbook_html.contains("@page unicell-sheet-0{size:"));
        assert!(workbook_html.contains("@page unicell-sheet-1{size:"));
        assert!(workbook_html.contains("data-workbook-pages="));
    }

    #[test]
    fn native_print_separates_area_unions_and_executes_page_setup_semantics() {
        let mut state = AppState::new();
        state.model.set_user_input(0, 1, 1, "=1/0").unwrap();
        state
            .model
            .set_user_input(0, 25, 13, "GAP_MUST_NOT_PRINT")
            .unwrap();
        state
            .model
            .set_user_input(0, 50, 26, "Second area")
            .unwrap();
        state.model.evaluate();
        let request = serde_json::to_vec(&json!({
            "op":"update",
            "patch":{"worksheets":[{
                "sheet":"Sheet1",
                "pageSetup":{"paperSize":12,"orientation":"portrait","pageOrder":"overThenDown",
                    "firstPageNumber":7,"useFirstPageNumber":true,"blackAndWhite":true,
                    "draft":true,"errors":"dash","cellComments":"atEnd"},
                "headerFooter":{"oddHeader":"&R&P/&N"},
                "printArea":"'Sheet1'!$A$1:$A$1,'Sheet1'!$Z$50:$Z$50",
                "notes":{"upsert":[{"ref":"A1","author":"Alice","text":"Check the formula"}]}
            }]}
        }))
        .unwrap();
        api_page_review(&mut state, &request).unwrap();
        let response = api_print_html_paged(
            &state,
            "sheet=0&paper=sheet&orientation=sheet&scaling=none&autoprint=0",
        )
        .unwrap();
        let mut html = String::new();
        response.into_reader().read_to_string(&mut html).unwrap();
        assert!(html.contains("data-print-area-count=\"2\""));
        assert!(html.contains("data-print-paper=\"B4 JIS\""));
        assert!(html.contains("data-page-order=\"overThenDown\""));
        assert!(html.contains("data-first-page-number=\"7\""));
        assert!(html.contains("data-black-and-white=\"true\""));
        assert!(html.contains("data-draft=\"true\""));
        assert!(html.contains("data-print-errors=\"dash\""));
        assert!(html.contains("data-print-comments=\"atEnd\""));
        assert!(html.contains("data-print-area=\"0\""));
        assert!(html.contains("data-print-area=\"1\""));
        assert!(html.contains("data-page-kind=\"comments\""));
        assert!(html.contains("Check the formula"));
        assert!(html.contains(">7/3<"));
        assert!(html.contains("--"));
        assert!(!html.contains("#DIV/0!"));
        assert!(!html.contains("GAP_MUST_NOT_PRINT"));
    }

    #[test]
    fn what_if_goal_seek_previews_in_isolation_and_applies_as_one_undo_transaction() {
        let mut state = AppState::new();
        state.model.set_user_input(0, 1, 1, "1").unwrap();
        state.model.set_user_input(0, 1, 2, "=A1*A1").unwrap();
        state.clear_application_history();
        let goal = json!({
            "kind":"goalSeek",
            "request":{
                "target":{"sheet":0,"row":1,"column":2},
                "changing":{"sheet":0,"row":1,"column":1},
                "targetValue":25,
                "lowerBound":0,"upperBound":10,
                "maxIterations":100,"tolerance":1e-10
            }
        });
        let mut preview_request = goal.clone();
        preview_request["op"] = Value::String("preview".into());
        let preview = serde_json::to_vec(&preview_request).unwrap();
        handle_api_with_history(&mut state, "/api/what-if", "", &preview).unwrap();
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "1");
        assert!(state.app_undo.is_empty());

        let mut apply_request = goal;
        apply_request["op"] = Value::String("apply".into());
        let apply = serde_json::to_vec(&apply_request).unwrap();
        handle_api_with_history(&mut state, "/api/what-if", "", &apply).unwrap();
        let changed = state
            .model
            .get_model()
            .get_cell_value_by_index(0, 1, 1)
            .unwrap();
        assert!(matches!(changed, CellValue::Number(value) if (value - 5.0).abs() < 1e-7));
        assert_eq!(state.app_undo.len(), 1);
        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "1");
    }

    #[test]
    fn what_if_data_table_and_scenario_manager_share_the_unified_history() {
        let mut state = AppState::new();
        state.model.set_user_input(0, 1, 1, "2").unwrap();
        state.model.set_user_input(0, 1, 2, "=A1*3").unwrap();
        state.clear_application_history();
        let table = serde_json::to_vec(&json!({
            "op":"apply","kind":"dataTable","request":{
                "kind":"oneVariable",
                "formulaCell":{"sheet":0,"row":1,"column":2},
                "inputCell":{"sheet":0,"row":1,"column":1},
                "values":[1,2,4],"orientation":"column",
                "output":{"sheet":0,"row":4,"column":1}
            }
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/what-if", "", &table).unwrap();
        assert_eq!(state.model.get_formatted_cell_value(0, 7, 2).unwrap(), "12");
        assert_eq!(state.app_undo.len(), 1);
        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 4, 1).unwrap(), "");

        let create = serde_json::to_vec(&json!({
            "op":"create","kind":"scenario","scenario":{
                "name":"High growth","comment":"fixture","changes":[{
                    "cell":{"sheet":0,"row":1,"column":1},"input":"8"
                }]
            }
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/what-if", "", &create).unwrap();
        let scenario_id = state.what_if_scenarios.list()[0].id.clone();
        let exported = api_export(&state, "", &[]).unwrap();
        let mut exported_bytes = Vec::new();
        exported
            .into_reader()
            .read_to_end(&mut exported_bytes)
            .unwrap();
        let exported_package = snapshot_opc_package(&exported_bytes).unwrap();
        let exported_sheet = snapshot_workbook_sheet_paths(&exported_package)[0].clone();
        let exported_xml = std::str::from_utf8(&exported_package.parts[&exported_sheet]).unwrap();
        assert!(exported_xml.contains("<scenarios"));
        assert!(exported_xml.contains("name=\"High growth\""));
        assert!(exported_xml.contains("<inputCells r=\"A1\" val=\"8\"/>"));
        let apply =
            serde_json::to_vec(&json!({"op":"apply","kind":"scenario","id":scenario_id})).unwrap();
        handle_api_with_history(&mut state, "/api/what-if", "", &apply).unwrap();
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "8");
        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "2");
        assert!(state.undo_application().unwrap());
        assert!(state.what_if_scenarios.list().is_empty());
    }

    #[test]
    fn ai_context_serves_digest_slice_detail_and_errors_with_a1_refs() {
        let mut state = AppState::new();
        state.model.set_user_input(0, 1, 1, "Amount").unwrap();
        state.model.set_user_input(0, 2, 1, "4").unwrap();
        state.model.set_user_input(0, 2, 2, "=A2/0").unwrap();
        state.model.discard_all_history();

        let digest = serde_json::to_vec(&json!({"op":"digest","sheet":"Sheet1"})).unwrap();
        let digest = response_value(api_ai_context(&state, &digest).unwrap());
        assert_eq!(digest["sheet"], "Sheet1");
        assert_eq!(digest["usedRange"], "Sheet1!A1:B2");
        assert!(digest["tables"].is_array());
        assert!(digest["definedNames"].is_array());
        assert!(digest["conditionalFormats"].is_array());
        assert!(digest["dataValidations"].is_array());
        assert!(digest["pivotTables"].is_array());

        let slice = serde_json::to_vec(&json!({"op":"slice","ref":"Sheet1!A1:B2"})).unwrap();
        let slice = response_value(api_ai_context(&state, &slice).unwrap());
        assert_eq!(slice["range"], "Sheet1!A1:B2");
        assert_eq!(slice["formulas"]["B2"], "=A2/0");

        let detail = serde_json::to_vec(&json!({"op":"detail","ref":"B2"})).unwrap();
        let detail = response_value(api_ai_context(&state, &detail).unwrap());
        assert_eq!(detail["ref"], "Sheet1!B2");
        assert_eq!(detail["precedents"], json!(["Sheet1!A2"]));
        assert_eq!(detail["kind"], "error");

        let errors = serde_json::to_vec(&json!({"op":"errors","sheet":0,"limit":10})).unwrap();
        let errors = response_value(api_ai_context(&state, &errors).unwrap());
        assert_eq!(errors["errors"][0]["ref"], "Sheet1!B2");
        assert_eq!(errors["errors"][0]["value"], "#DIV/0!");
    }

    #[test]
    fn ai_apply_dry_run_is_isolated_and_confirmed_batch_is_one_undo_step() {
        let mut state = AppState::new();
        state.worksheet_features.data_validations.insert(
            0,
            DataValidationSheet {
                container_start_tag: String::new(),
                rules: vec![DataValidationRule {
                    id: "dv-ai-a1".into(),
                    sqref: "A1".into(),
                    validation_type: "whole".into(),
                    operator: Some("between".into()),
                    allow_blank: false,
                    in_cell_dropdown: true,
                    show_input_message: false,
                    show_error_message: true,
                    error_style: None,
                    ime_mode: None,
                    prompt_title: None,
                    prompt: None,
                    error_title: None,
                    error: None,
                    formula1: Some("1".into()),
                    formula2: Some("3".into()),
                    raw_xml: None,
                }],
            },
        );
        let before_bytes = state.model.to_bytes();
        let before_undo = state.model.undo_depth();
        let preview = serde_json::to_vec(&json!({
            "ops": [
                {"op":"setValue","ref":"A1","value":4},
                {"op":"setFormula","ref":"B1","formula":"=A1/0"}
            ]
        }))
        .unwrap();
        let preview = response_value(
            handle_api_with_history(&mut state, "/api/ai/apply", "", &preview).unwrap(),
        );
        assert_eq!(preview["dryRun"], true);
        assert_eq!(preview["applied"], false);
        assert_eq!(preview["changedCells"], 2);
        assert_eq!(preview["errors"][0]["ref"], "Sheet1!B1");
        assert_eq!(preview["validationViolations"][0]["ref"], "Sheet1!A1");
        assert_eq!(
            preview["validationViolations"][0]["reason"],
            "comparisonFailed"
        );
        assert_eq!(state.model.to_bytes(), before_bytes);
        assert_eq!(state.model.undo_depth(), before_undo);
        assert!(state.app_undo.is_empty());

        let apply = serde_json::to_vec(&json!({
            "dryRun": false,
            "ops": [
                {"op":"setValue","ref":"A1","value":4},
                {"op":"setFormula","ref":"B1","formula":"=A1/0"}
            ]
        }))
        .unwrap();
        let applied = response_value(
            handle_api_with_history(&mut state, "/api/ai/apply", "", &apply).unwrap(),
        );
        assert_eq!(applied["applied"], true);
        assert_eq!(applied["errors"][0]["value"], "#DIV/0!");
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "4");
        assert_eq!(state.model.get_cell_content(0, 1, 2).unwrap(), "=A1/0");
        assert_eq!(state.app_undo.len(), 1);

        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "");
        assert_eq!(state.model.get_cell_content(0, 1, 2).unwrap(), "");
    }

    #[test]
    fn ai_apply_rejects_the_whole_batch_before_writing_and_targets_merge_anchors() {
        let mut state = AppState::new();
        state.model.merge_cells_range(0, 1, 1, 2, 2).unwrap();
        state.model.discard_all_history();
        state.clear_application_history();

        let invalid = serde_json::to_vec(&json!({
            "dryRun": false,
            "ops": [
                {"op":"setValue","ref":"A1","value":"must-not-land"},
                {"op":"setFormula","ref":"C1","formula":"SUM(A1)"}
            ]
        }))
        .unwrap();
        assert!(handle_api_with_history(&mut state, "/api/ai/apply", "", &invalid).is_err());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "");
        assert!(state.app_undo.is_empty());

        let merged = serde_json::to_vec(&json!({
            "dryRun": false,
            "ops": [{"op":"setValue","ref":"B2","value":"=literal"}]
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/ai/apply", "", &merged).unwrap();
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "=literal");
        assert_eq!(
            state.model.get_formatted_cell_value(0, 1, 1).unwrap(),
            "=literal"
        );
        assert_eq!(state.app_undo.len(), 1);
        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "");
    }

    #[test]
    fn ai_typed_ops_preview_and_commit_format_and_native_chart_as_one_undo_step() {
        let original = native_drawing_fixture();
        let snapshot = snapshot_opc_package(&original).unwrap();
        let files: std::collections::HashMap<String, Vec<u8>> = snapshot
            .parts
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let drawing = std::str::from_utf8(&files["xl/drawings/drawing1.xml"]).unwrap();
        let rels = parse_rels_map(
            std::str::from_utf8(&files["xl/drawings/_rels/drawing1.xml.rels"]).unwrap(),
        );
        let mut state = AppState::new();
        state.model.set_user_input(0, 1, 1, "Revenue").unwrap();
        let objects = parse_drawing_pics(
            drawing,
            &rels,
            "xl/drawings/drawing1.xml",
            &files,
            0,
            &state.model,
        );
        let chart_id = objects
            .iter()
            .find(|object| object["config"]["nativeDrawing"]["kind"] == "chart")
            .and_then(|object| object["id"].as_str())
            .unwrap()
            .to_string();
        state.objects.insert(0, objects);
        state.source_ooxml = Some(snapshot);
        state.model.discard_all_history();
        state.clear_application_history();

        let baseline_style = style_to_json(&state, &state.model.get_cell_style(0, 1, 1).unwrap());
        let baseline_chart = ai_chart_model_from_object(
            &state,
            state.objects[&0]
                .iter()
                .find(|object| object["id"] == chart_id)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(baseline_chart["title"], "季度收入");

        let digest = response_value(
            api_ai_context(
                &state,
                &serde_json::to_vec(&json!({"op":"digest"})).unwrap(),
            )
            .unwrap(),
        );
        assert_eq!(digest["charts"][0]["id"], chart_id);
        assert_eq!(digest["charts"][0]["model"]["title"], "季度收入");

        let mut request = json!({
            "dryRun":true,
            "ops":[
                {"op":"setFormat","ref":"Sheet1!A1","style":{
                    "font.bold":true,"fill.color":"#E8F5E9","numberFormat":"#,##0.00"
                }},
                {"op":"updateChart","sheet":"Sheet1","chartId":chart_id,
                    "patch":{"title":"AI 销售趋势","legend":{"show":true,"position":"bottom"}}}
            ]
        });
        let preview = response_value(
            handle_api_with_history(
                &mut state,
                "/api/ai/apply",
                "",
                &serde_json::to_vec(&request).unwrap(),
            )
            .unwrap(),
        );
        assert_eq!(preview["changedCells"], 1);
        assert_eq!(preview["changedObjects"], 1);
        assert_ne!(
            preview["diff"][0]["before"]["style"],
            preview["diff"][0]["after"]["style"]
        );
        assert_eq!(
            preview["objectDiff"][0]["after"]["model"]["title"],
            "AI 销售趋势"
        );
        assert_eq!(
            style_to_json(&state, &state.model.get_cell_style(0, 1, 1).unwrap()),
            baseline_style
        );
        assert_eq!(
            ai_chart_model_from_object(
                &state,
                state.objects[&0]
                    .iter()
                    .find(|object| object["id"] == chart_id)
                    .unwrap(),
            )
            .unwrap()["title"],
            "季度收入"
        );
        assert!(state.app_undo.is_empty());

        request["dryRun"] = Value::Bool(false);
        let applied = response_value(
            handle_api_with_history(
                &mut state,
                "/api/ai/apply",
                "",
                &serde_json::to_vec(&request).unwrap(),
            )
            .unwrap(),
        );
        assert_eq!(applied["changedCells"], 1);
        assert_eq!(applied["changedObjects"], 1);
        assert_ne!(
            style_to_json(&state, &state.model.get_cell_style(0, 1, 1).unwrap()),
            baseline_style
        );
        assert_eq!(
            ai_chart_model_from_object(
                &state,
                state.objects[&0]
                    .iter()
                    .find(|object| object["id"] == chart_id)
                    .unwrap(),
            )
            .unwrap()["title"],
            "AI 销售趋势"
        );
        assert_eq!(state.app_undo.len(), 1);

        assert!(state.undo_application().unwrap());
        assert_eq!(
            style_to_json(&state, &state.model.get_cell_style(0, 1, 1).unwrap()),
            baseline_style
        );
        assert_eq!(
            ai_chart_model_from_object(
                &state,
                state.objects[&0]
                    .iter()
                    .find(|object| object["id"] == chart_id)
                    .unwrap(),
            )
            .unwrap()["title"],
            "季度收入"
        );
    }

    #[test]
    fn ai_typed_pivot_update_uses_stable_part_preview_and_one_step_undo() {
        let mut state = AppState::new();
        state.source_ooxml = Some(OpcPackageSnapshot {
            parts: std::collections::BTreeMap::from([
                (
                    "_rels/.rels".to_string(),
                    br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_vec(),
                ),
                (
                    "xl/workbook.xml".to_string(),
                    br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="sheet1"/></sheets></workbook>"#.to_vec(),
                ),
                (
                    "xl/_rels/workbook.xml.rels".to_string(),
                    br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheet1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.to_vec(),
                ),
                (
                    "xl/worksheets/sheet1.xml".to_string(),
                    br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><pivotTableParts count="1"><pivotTablePart r:id="pivot1"/></pivotTableParts></worksheet>"#.to_vec(),
                ),
                (
                    "xl/worksheets/_rels/sheet1.xml.rels".to_string(),
                    br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="pivot1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../pivotTables/pivotTable1.xml"/></Relationships>"#.to_vec(),
                ),
                (
                    "xl/pivotTables/pivotTable1.xml".to_string(),
                    br#"<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x="urn:vendor" name="SalesPivot" cacheId="1" compact="1" outline="0" rowGrandTotals="1" x:keep="opaque"><location ref="A3:D10"/><pivotFields count="1"><pivotField showAll="1"/></pivotFields><rowFields count="1"><field x="0"/></rowFields><extLst><x:future answer="42"/></extLst></pivotTableDefinition>"#.to_vec(),
                ),
            ]),
            macro_enabled: false,
        });
        state.clear_application_history();
        let part = "xl/pivotTables/pivotTable1.xml";

        let digest = response_value(
            api_ai_context(
                &state,
                &serde_json::to_vec(&json!({"op":"digest"})).unwrap(),
            )
            .unwrap(),
        );
        assert_eq!(digest["pivotTables"][0]["part"], part);
        assert_eq!(
            digest["pivotTables"][0]["model"]["display"]["compact"],
            true
        );

        let mut request = json!({
            "dryRun":true,
            "ops":[{"op":"updatePivotTable","part":part,"patch":{
                "display":{"compact":false,"outline":true,"rowGrandTotals":false}
            }}]
        });
        let preview = response_value(
            handle_api_with_history(
                &mut state,
                "/api/ai/apply",
                "",
                &serde_json::to_vec(&request).unwrap(),
            )
            .unwrap(),
        );
        assert_eq!(preview["changedCells"], 0);
        assert_eq!(preview["changedObjects"], 1);
        assert_eq!(
            preview["objectDiff"][0]["after"]["model"]["display"]["compact"],
            false
        );
        assert!(state.native_pivot_table_edits.is_empty());
        assert!(state.app_undo.is_empty());

        request["dryRun"] = Value::Bool(false);
        let applied = response_value(
            handle_api_with_history(
                &mut state,
                "/api/ai/apply",
                "",
                &serde_json::to_vec(&request).unwrap(),
            )
            .unwrap(),
        );
        assert_eq!(applied["changedObjects"], 1);
        let edited = pivot_table_model_with_edits(&state).unwrap();
        assert_eq!(edited["tables"][0]["display"]["compact"], false);
        assert_eq!(edited["tables"][0]["display"]["outline"], true);
        assert_eq!(state.app_undo.len(), 1);

        assert!(state.undo_application().unwrap());
        let undone = pivot_table_model_with_edits(&state).unwrap();
        assert_eq!(undone["tables"][0]["display"]["compact"], true);
        assert_eq!(undone["tables"][0]["display"]["outline"], false);
    }

    #[test]
    fn ai_typed_ops_obey_the_same_sheet_protection_as_native_endpoints() {
        let mut state = AppState::new();
        let protect = serde_json::to_vec(&json!({
            "op":"update",
            "patch":{"worksheets":[{
                "sheet":"Sheet1",
                "sheetProtection":{"sheet":true}
            }]}
        }))
        .unwrap();
        handle_api_with_history(&mut state, "/api/page-review", "", &protect).unwrap();
        state.clear_application_history();

        for operation in [
            json!({"op":"setValue","ref":"Sheet1!A1","value":"blocked"}),
            json!({"op":"setFormat","ref":"Sheet1!A1","style":{"font.bold":true}}),
        ] {
            let request = serde_json::to_vec(&json!({"dryRun":false,"ops":[operation]})).unwrap();
            let error = match handle_api_with_history(&mut state, "/api/ai/apply", "", &request) {
                Ok(_) => panic!("AI typed op bypassed worksheet protection"),
                Err(error) => error,
            };
            assert!(
                error.contains("protection") || error.contains("保护"),
                "{error}"
            );
        }
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "");
        assert_eq!(
            style_to_json(&state, &state.model.get_cell_style(0, 1, 1).unwrap())["b"],
            false
        );
        assert!(state.app_undo.is_empty());
    }

    #[test]
    fn udoc_hybrid_compression_selects_brotli_and_single_entry_zip() {
        use std::io::Cursor;

        let manifest = br#"{"format":"udoc-package","version":3}"#.repeat(64);
        let svg =
            br#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h10v10z"/></svg>"#.repeat(64);
        let png = (0..=255).cycle().take(32 * 1024).collect::<Vec<_>>();
        let wav = vec![0u8; 32 * 1024];
        let workbook = b"PK\x03\x04already-compressed-xlsx".repeat(128);
        let tiny = b"legacy-store".to_vec();
        let parts = vec![
            (
                "manifest.json".into(),
                manifest.clone(),
                "application/json".into(),
            ),
            (
                "media/vector.svg".into(),
                svg.clone(),
                "image/svg+xml".into(),
            ),
            ("media/photo.png".into(), png.clone(), "image/png".into()),
            ("media/audio.wav".into(), wav.clone(), "audio/wav".into()),
            (
                "document/workbook.xlsx".into(),
                workbook.clone(),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
            ),
            (
                "document/tiny.txt".into(),
                tiny.clone(),
                "text/plain".into(),
            ),
        ];
        let package = encode_udoc3(parts.clone()).unwrap();
        assert_eq!(package, encode_udoc3(parts).unwrap());
        let (_, directory) = decode_udoc3_directory(&package).unwrap();
        let entry = |path: &str| {
            directory
                .entries
                .iter()
                .find(|entry| entry.path == path)
                .unwrap()
        };
        assert_eq!(entry("manifest.json").codec, "br");
        assert_eq!(entry("media/vector.svg").codec, "br");
        assert_eq!(entry("media/photo.png").codec, "zip");
        assert_eq!(entry("media/audio.wav").codec, "zip");
        assert_eq!(entry("document/workbook.xlsx").codec, "zip");
        assert_eq!(entry("document/tiny.txt").codec, "store");
        assert!(entry("manifest.json").compressed_size < manifest.len() as u64);
        assert!(entry("media/vector.svg").compressed_size < svg.len() as u64);
        assert!(entry("media/audio.wav").compressed_size < wav.len() as u64);
        let raw_size =
            manifest.len() + svg.len() + png.len() + wav.len() + workbook.len() + tiny.len();
        assert!(package.len() < raw_size);

        for (path, expected_method) in [
            ("media/photo.png", zip::CompressionMethod::Stored),
            ("media/audio.wav", zip::CompressionMethod::Deflated),
            ("document/workbook.xlsx", zip::CompressionMethod::Stored),
        ] {
            let descriptor = entry(path);
            let start = descriptor.offset as usize;
            let end = start + descriptor.compressed_size as usize;
            let mut archive =
                zip::read::ZipArchive::new(Cursor::new(&package[start..end])).unwrap();
            assert_eq!(archive.len(), 1);
            let file = archive.by_index(0).unwrap();
            assert_eq!(file.name(), path);
            assert_eq!(file.compression(), expected_method);
        }

        let decoded = decode_udoc3(&package).unwrap();
        assert_eq!(decoded["manifest.json"], manifest);
        assert_eq!(decoded["media/vector.svg"], svg);
        assert_eq!(decoded["media/photo.png"], png);
        assert_eq!(decoded["media/audio.wav"], wav);
        assert_eq!(decoded["document/workbook.xlsx"], workbook);
        assert_eq!(decoded["document/tiny.txt"], tiny);
    }

    #[test]
    fn udoc_decoder_accepts_legacy_stored_binary_and_rejects_tampering() {
        let path = "media/legacy.bin";
        let bytes = b"legacy UDOC3 stored media".to_vec();
        let mut package = UDOC3_HEADER.to_vec();
        let offset = package.len() as u64;
        package.extend_from_slice(&bytes);
        let directory_json = serde_json::to_vec(&json!({
            "format":"udoc-directory",
            "version":3,
            "entries":[{
                "path":path,
                "offset":offset,
                "compressedSize":bytes.len(),
                "size":bytes.len(),
                "codec":"store",
                "mime":"application/octet-stream",
                "sha256":sha256_hex(&bytes)
            }]
        }))
        .unwrap();
        let directory_packed = br_compress(&directory_json).unwrap();
        let directory_offset = package.len() as u64;
        package.extend_from_slice(&directory_packed);
        let mut footer = vec![0u8; UDOC3_FOOTER_SIZE];
        footer[0..8].copy_from_slice(UDOC3_FOOTER_MAGIC);
        footer[8..16].copy_from_slice(&directory_offset.to_le_bytes());
        footer[16..24].copy_from_slice(&(directory_packed.len() as u64).to_le_bytes());
        footer[24..56].copy_from_slice(&sha2::Sha256::digest(&directory_json));
        footer[56..60].copy_from_slice(&3u32.to_le_bytes());
        footer[60..64].copy_from_slice(&(directory_json.len() as u32).to_le_bytes());
        package.extend_from_slice(&footer);

        assert_eq!(decode_udoc3(&package).unwrap()[path], bytes);
        let mut tampered = package.clone();
        tampered[offset as usize] ^= 0x01;
        assert!(decode_udoc3(&tampered).unwrap_err().contains("校验失败"));
        assert!(
            encode_udoc3(vec![(
                "../escape.json".into(),
                b"{}".to_vec(),
                "application/json".into(),
            )])
            .unwrap_err()
            .contains("路径无效")
        );
    }

    #[test]
    fn udoc_exports_compact_derived_views_and_imports_only_the_authoritative_workbook() {
        let mut state = AppState::new();
        state.model.set_user_input(0, 5, 3, "Amount").unwrap();
        state.model.set_user_input(0, 6, 3, "7").unwrap();
        state.model.set_user_input(0, 6, 4, "=C6*2").unwrap();
        state.model.discard_all_history();
        let shared_image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB";
        state.objects.insert(
            0,
            vec![
                json!({"id":"image-a","type":"image","config":{"src":shared_image}}),
                json!({"id":"image-b","type":"image","config":{"src":shared_image}}),
            ],
        );

        let response = api_export_udoc(&state, "name=ai-friendly").unwrap();
        let mut package = Vec::new();
        response.into_reader().read_to_end(&mut package).unwrap();
        let parts = decode_udoc3(&package).unwrap();
        let manifest: Value = serde_json::from_slice(&parts["manifest.json"]).unwrap();
        assert_eq!(manifest["authoritative"], "document/workbook.xlsx");
        assert_eq!(manifest["derivedViews"]["schema"], "unicell-row-major-v2");
        assert_eq!(manifest["compression"]["strategy"], "hybrid-br-zip");
        assert_eq!(manifest["compression"]["wholeFile"], false);
        assert!(
            manifest["features"]
                .as_array()
                .unwrap()
                .iter()
                .any(|feature| feature == "hybrid-br-zip")
        );
        let (_, directory) = decode_udoc3_directory(&package).unwrap();
        assert_eq!(
            directory
                .entries
                .iter()
                .find(|entry| entry.path == "document/workbook.xlsx")
                .unwrap()
                .codec,
            "zip"
        );
        let media_entries = directory
            .entries
            .iter()
            .filter(|entry| entry.path.starts_with("media/"))
            .collect::<Vec<_>>();
        assert_eq!(media_entries.len(), 1);
        assert_eq!(media_entries[0].codec, "zip");
        let relationships: Value =
            serde_json::from_slice(&parts["rels/relationships.json"]).unwrap();
        assert_eq!(relationships["relationships"].as_array().unwrap().len(), 2);

        let digest: Value = serde_json::from_slice(&parts["document/digest.json"]).unwrap();
        assert!(digest["sheets"].is_array());
        assert!(digest["tables"].is_array());
        assert!(digest["definedNames"].is_array());

        let chunk: Value = serde_json::from_slice(&parts["document/chunks/sheet0.json"]).unwrap();
        assert_eq!(chunk["schema"], "unicell-row-major-v2");
        assert_eq!(chunk["layout"], "dense");
        assert_eq!(chunk["derived"], true);
        assert_eq!(chunk["authoritative"], false);
        assert_eq!(chunk["origin"], "C5");
        assert!(chunk.get("cells").is_none());
        assert_eq!(chunk["rows"][0][0], "Amount");
        assert_eq!(chunk["rows"][1][1], "=C6*2");
        assert_eq!(chunk["display"]["D6"], "14");
        assert!(chunk["display"].get("C5").is_none());

        let mut no_derived_bytes = Vec::new();
        api_export_udoc(&state, "name=lean&derived=0")
            .unwrap()
            .into_reader()
            .read_to_end(&mut no_derived_bytes)
            .unwrap();
        let no_derived = decode_udoc3(&no_derived_bytes).unwrap();
        assert!(!no_derived.contains_key("document/digest.json"));
        assert!(!no_derived.contains_key("document/chunks/sheet0.json"));
        let no_derived_manifest: Value =
            serde_json::from_slice(&no_derived["manifest.json"]).unwrap();
        assert!(no_derived_manifest["derivedViews"].is_null());

        let legacy_manifest = json!({
            "format": "udoc-package",
            "version": 3,
            "unidoc_type": "cell",
            "basename": "legacy-compatible"
        });
        let legacy_document = json!({"sheets": []});
        let legacy_chunk = json!({
            "sheet": 0,
            "cells": [{"r": 6, "c": 4, "v": "CORRUPT", "f": "CORRUPT"}]
        });
        let legacy_package = encode_udoc3(vec![
            (
                "manifest.json".into(),
                serde_json::to_vec(&legacy_manifest).unwrap(),
                "application/json".into(),
            ),
            (
                "document/workbook.xlsx".into(),
                parts["document/workbook.xlsx"].clone(),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
            ),
            (
                "document/document.json".into(),
                serde_json::to_vec(&legacy_document).unwrap(),
                "application/json".into(),
            ),
            (
                "document/chunks/sheet0.json".into(),
                serde_json::to_vec(&legacy_chunk).unwrap(),
                "application/json".into(),
            ),
        ])
        .unwrap();
        let mut imported = AppState::new();
        api_import_udoc(&mut imported, &legacy_package).unwrap();
        assert_eq!(imported.model.get_cell_content(0, 6, 4).unwrap(), "=C6*2");
        assert_ne!(imported.model.get_cell_content(0, 6, 4).unwrap(), "CORRUPT");
    }

    #[test]
    fn udoc_sparse_chunk_never_materializes_a_huge_null_rectangle() {
        let mut state = AppState::new();
        state.model.set_user_input(0, 1, 1, "near").unwrap();
        state
            .model
            .set_user_input(0, MAX_ROWS, MAX_COLS, "far")
            .unwrap();
        let chunk = udoc_sheet_chunk(&state, 0, "Sheet1").unwrap();
        assert_eq!(chunk["schema"], "unicell-row-major-v2");
        assert_eq!(chunk["layout"], "sparse");
        assert_eq!(chunk["origin"], "A1");
        assert_eq!(chunk["shape"], json!([MAX_ROWS, MAX_COLS]));
        assert_eq!(chunk["populated"], 2);
        assert_eq!(chunk["rows"].as_array().unwrap().len(), 2);
        assert_eq!(chunk["rows"][0], json!([0, [[0, "near"]]]));
        assert_eq!(
            chunk["rows"][1],
            json!([MAX_ROWS - 1, [[MAX_COLS - 1, "far"]]])
        );
    }
}
