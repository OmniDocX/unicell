# Feature gaps relative to Excel

**English** | [简体中文](EXCEL_GAPS.zh-CN.md)

[Project overview](README.md) · [Features and limitations](docs/FEATURES.md) · [Product comparison](docs/COMPARISON.md)

## About this list

This list uses the native features of Excel for Microsoft 365 on the desktop as the reference and records what the UniCell public local edition does not yet cover. It was compiled from a source-code review, not from interface trials or inference from documentation.

| Item | Value |
| --- | --- |
| Baseline | Commit `9a45c5f94da22cc78cd9654258c334ebef8ff15c` |
| Review date | 2026-09-17 |
| Scope | `web/` (interface), `server/src/` (service), `server/vendor/ironcalc_base/` (calculation engine) |
| Method | Each item was checked against controls, endpoints and implementations; every "Not implemented" entry is based on keyword searches that returned no match |
| Out of scope | `vecmeta/`; differences among Excel for the web, mobile and Mac; performance comparison |

Status labels:

| Status | Meaning |
| --- | --- |
| Not implemented | No corresponding implementation in the interface, service or engine |
| Preserved only | The OOXML part is retained byte-for-byte on import and export; its model cannot be read or edited and it does not take part in calculation |
| Existing objects only | Objects already present in an imported file can be edited; new ones cannot be created |
| Partial | Implemented with a narrower scope than Excel; see the notes column |
| No interface | Supported by the service or engine but not reachable from the interface |

On export the calculation engine regenerates the workbook, after which non-controlled parts are written back byte-for-byte (`opc_part_is_controlled` and `opc_part_is_preservable` in `server/src/main.rs`). "The file is not damaged" and "the feature is usable" are therefore distinct levels, and this list labels them separately.

## Overview

| Area | Available | Main gaps |
| --- | --- | --- |
| Files and interoperability | XLSX/XLSM, CSV, UDOC, UniCell HTML | XLS, XLSB, ODS, Text Import Wizard, file encryption, PDF export |
| Cell editing and formatting | Fonts, fills, borders, merging, character-level rich text, Format Painter, find and replace | Custom number format entry, indent and text rotation, cell styles, Go To Special, fill series, multiple-range selection |
| Formulas and calculation | 497 functions, dynamic arrays, LAMBDA, iterative calculation, manual calculation | 28 functions, external workbook references, incremental recalculation, 1904 date system, Chinese locale and Chinese numeral formats |
| Data tools | Multi-level sorting, AutoFilter, data validation, all three what-if analysis tools | Header filter drop-downs, Text to Columns, Remove Duplicates, Consolidate, Subtotal, Outline, Advanced Filter, Flash Fill, Solver |
| Tables and PivotTables | Table creation and calculated columns; editing and local refresh of existing PivotTables | New PivotTables, calculated fields, grouping, GETPIVOTDATA, PivotCharts, new slicers and timelines |
| Charts and graphics | Property editing for 10 classic chart types; editing of existing shapes and SmartArt | New charts, chart types introduced in Excel 2016, sparklines, shape gallery, WordArt, picture editing |
| Page layout and printing | Page setup, headers and footers, page breaks, print titles, browser printing | Document themes, sheet background, Page Layout view, Page Break Preview driven by page setup |
| Review and protection | Notes, threaded comments, sheet and workbook protection, Allow Edit Ranges | Spelling, Track Changes, comments shown on the grid, file encryption, digital signatures |
| View and window | Freeze Panes, zoom, gridline and formula bar toggles | Split, New Window and View Side by Side, Custom Views, headings toggle |
| Automation and extensibility | Local MCP, optional AI | VBA execution, macro recording, Office Scripts, add-ins, form controls |
| Interaction and internationalization | Keyboard navigation, command palette, dark theme | Several common shortcuts, Alt KeyTips, interface languages, grid accessibility semantics, touch gestures |

## 1. Files and interoperability

| Excel feature | Status | Notes |
| --- | --- | --- |
| XLS (BIFF binary), XLSB, ODS | Not implemented | No import or export path |
| XLTX / XLTM templates | Partial | Opened as ordinary workbooks; no "new from template" semantics |
| Text Import Wizard | Not implemented | CSV is always comma-delimited; delimiters, fixed width and per-column data types cannot be specified |
| CSV encoding selection | Partial | Import detects UTF-8, UTF-16 and GBK in that order and cannot be overridden; export is always UTF-8 with BOM |
| TSV / TXT files | Not implemented | Tab-delimited text is used for the clipboard only |
| Import of arbitrary HTML tables | Not implemented | HTML import accepts only files exported by UniCell |
| PDF export | Not implemented | The service does not generate PDF; it is obtained through browser printing |
| Opening or saving workbooks with an open password | Not implemented | No reading or writing of OOXML encrypted packages (Agile / Standard) |
| Digital signatures | Not implemented | Signature parts are removed on export |
| External workbook links | Preserved only | `externalLink` parts are shown read-only; references are not evaluated |
| OLE objects, ActiveX controls, form controls | Preserved only | Cannot be created, edited or operated |
| AutoSave and version history | Not implemented | Browser recovery copies are supplementary only |

## 2. Cell editing and formatting

### 2.1 Font, alignment and borders

| Excel feature | Status | Notes |
| --- | --- | --- |
| Font size | Partial | The ribbon offers 13 fixed sizes; arbitrary values cannot be entered |
| Underline styles, superscript, subscript | Not implemented | Underline is an on/off toggle; no double or accounting underline |
| Pattern and gradient fills | Not implemented | Cell fills are solid colors only |
| Diagonal borders, Draw Border | Not implemented | `/api/border` types do not include diagonals |
| Border line styles | Partial | The dialog offers 5 line styles; the ribbon drop-down always applies thin black lines |
| Increase / Decrease Indent | Not implemented | — |
| Orientation and text rotation | Not implemented | — |
| Shrink to Fit | Not implemented | — |
| Justify, Distributed, Center Across Selection, Fill alignment | Not implemented | Horizontal alignment offers General, Left, Center and Right; vertical alignment offers 3 options |
| Right-to-left text direction | Not implemented | — |
| Merge & Center, Merge Across | Not implemented | A single toggle merges or unmerges |

### 2.2 Number formats and styles

| Excel feature | Status | Notes |
| --- | --- | --- |
| Custom number format codes | No interface | `num_fmt` in `/api/style` accepts any format string; the interface offers 12 fixed options |
| Accounting, Fraction and Special categories | Not implemented | The engine renders the fraction format `# ?/?` as an ordinary number followed by a slash |
| `[DBNum1]` to `[DBNum3]` Chinese numeral formats | Not implemented | The format lexer does not recognize the token |
| Locale tags such as `[$-804]` | Partial | The engine ignores the locale segment; the currency symbol is limited to one character |
| Negative number display options | Not implemented | — |
| Cell Styles gallery | Not implemented | No named styles |
| Format as Table | Partial | The table style name is written to OOXML but not rendered on the grid; no gallery preview and no custom table styles |

### 2.3 Conditional formatting

The Rules Manager supports priority changes, duplication, deletion, editing of the applies-to range and Stop If True. The gaps concern rule creation and display.

| Excel feature | Status | Notes |
| --- | --- | --- |
| New rule | Partial | The interface creates 6 kinds: greater than, less than, equal to, between, text contains, duplicate values; formatting is limited to 3 fixed presets |
| Data bars, color scales, icon sets | Existing objects only | Can be listed, reordered and deleted; cannot be created and are not rendered on the grid |
| Formula rules, top/bottom N, above/below average, date occurring, unique values, blanks and errors | Existing objects only | Evaluated by the service; no creation entry in the interface |
| x14 extension rules | Preserved only | The interface states this boundary |

### 2.4 Editing operations

| Excel feature | Status | Notes |
| --- | --- | --- |
| Paste Special | Partial | All, values, formulas, formats and their transposed variants; no operations (add, subtract, multiply, divide), Skip Blanks, column widths, comments, Paste Link or paste as picture |
| Office Clipboard (multiple items) | Not implemented | — |
| Find and Replace | Partial | Match case and match entire cell contents; no workbook scope, wildcards, find by format, search direction or Find All result list |
| Go To Special | Not implemented | No F5 / Ctrl+G |
| Fill Down / Fill Right commands | Not implemented | No Ctrl+D / Ctrl+R |
| Series dialog | Not implemented | Dragging and double-clicking the fill handle are available |
| Auto Fill Options | Not implemented | — |
| Flash Fill | Not implemented | — |
| Clear Comments, Clear Hyperlinks | Not implemented | Clear offers contents, formats and all |
| Insert or delete cells (shifting adjacent cells) | Not implemented | Entire rows and columns only |
| Multiple-range selection | Not implemented | The selection is a single rectangle; Ctrl+click is not supported |
| AutoFit row height and column width | Not implemented | Double-clicking a boundary has no effect; sizes are entered in pixels |
| Hide rows and columns | Partial | Implemented by setting the size to zero rather than OOXML `hidden`; unhiding restores the default size |
| Format Painter lock (repeated use) | Not implemented | Exits after one use |
| Undo history list, Repeat | Not implemented | Undo and redo are available |

### 2.5 Worksheets

| Excel feature | Status | Notes |
| --- | --- | --- |
| Tab color | Not implemented | — |
| Hide and unhide sheets | Not implemented | `/api/sheet` operations are new, delete, rename, duplicate and move |
| Reorder sheets by dragging | No interface | The service provides `move`; the interface does not call it |
| Move or copy to another workbook | Not implemented | — |
| Sheet grouping (editing several sheets at once) | Not implemented | — |

## 3. Formulas and calculation

### 3.1 Functions

The calculation engine implements 497 functions, all with evaluation logic. Dynamic arrays, the LAMBDA family, XLOOKUP, TEXTSPLIT, GROUPBY, PIVOTBY, the REGEX family and the FORECAST.ETS family are available. Compared with the Microsoft 365 function list, the following 28 are missing:

| Category | Missing functions |
| --- | --- |
| Lookup and reference | HYPERLINK, GETPIVOTDATA, FIELDVALUE, IMAGE, RTD |
| Text | DBCS, JIS, PHONETIC, BAHTTEXT, TRANSLATE, DETECTLANGUAGE |
| Web | ENCODEURL, FILTERXML, WEBSERVICE |
| Financial | STOCKHISTORY |
| Cube | CUBEKPIMEMBER, CUBEMEMBER, CUBEMEMBERPROPERTY, CUBERANKEDMEMBER, CUBESET, CUBESETCOUNT, CUBEVALUE |
| Add-in and automation | CALL, EUROCONVERT, REGISTER.ID, SQL.REQUEST |
| Other | COPILOT, PY |

Some function arguments are not covered: `CELL` does not support `color`, `format`, `parentheses`, `prefix`, `protect` or `width`; `INFO("RECALC")` always returns `Automatic` regardless of manual calculation mode.

The interface function list (`web/functions.js`) differs from the engine: it contains 78 names the engine does not recognize (for example `NETWORKDAYSINTL` and `CHISQDIST`) and omits 81 functions the engine implements (names containing periods, plus `AGGREGATE`, `GROUPBY`, `PIVOTBY` and others). These functions can be typed manually but do not appear in AutoComplete or the Insert Function dialog.

### 3.2 Calculation semantics

| Excel feature | Status | Notes |
| --- | --- | --- |
| External workbook references | Not implemented | The reference model has no workbook dimension |
| Dependency-driven incremental recalculation | Not implemented | Each evaluation clears the cache and recalculates every cell; volatile functions are not distinguished |
| Multi-threaded calculation | Not implemented | — |
| 1904 date system | Not implemented | The date base is fixed at 1900 |
| Precision as displayed | Not implemented | — |
| Automatic Except for Data Tables | Not implemented | Treated as automatic on import |
| Trim reference operators such as `A1.:.B5` | Not implemented | The `TRIMRANGE` function is available |
| Structured references with multiple item specifiers | Not implemented | Single specifiers and the `@` form are available |
| 3-D references | Partial | Aggregate functions work; functions that require array conversion reject 3-D ranges |
| R1C1 reference style | Partial | Used for internal storage only; no user-facing switch |
| Circular reference handling | Partial | Iterative calculation is available; when disabled the result is `#CIRC!`, which is not a standard XLSX error value |
| Error values | Partial | All 9 standard errors are present; `#FIELD!`, `#BLOCKED!`, `#CONNECT!`, `#BUSY!` and similar are absent |
| Function-name and locale localization | No interface | The engine includes 5 languages and 6 locales, but the service always uses `en`; there is no Chinese locale, so month names, weekday names and currency formats follow the English locale |

### 3.3 Formula tools

| Excel feature | Status | Notes |
| --- | --- | --- |
| Function Arguments dialog and argument ScreenTips | Not implemented | AutoComplete shows function names only |
| Name Manager | Partial | List, create and delete; existing names cannot be edited, worksheet scope and comments cannot be set, and Create from Selection is absent |
| Trace Precedents | Partial | Parses the text of the current formula only; no multi-level tracing or cross-sheet references |
| Error Checking, Trace Error | Not implemented | — |
| Evaluate Formula | Not implemented | — |
| Watch Window | Not implemented | — |
| AutoSum drop-down (Average, Count Numbers, Max, Min) | Not implemented | Sum only |

## 4. Data tools

### 4.1 Sort and filter

| Excel feature | Status | Notes |
| --- | --- | --- |
| Filter drop-down buttons in headers | Not implemented | The grid draws no filter button; filters are defined in a dialog by column and criteria |
| Filter value list | Partial | Values are typed manually; distinct values are not listed and there is no search box |
| Text filters (contains, begins with, ends with) | No interface | The service implements these operators and wildcards; the interface drop-down offers 6 comparison operators |
| Filter by icon | Partial | Can be set and written to OOXML; not applied by the local filter |
| Reapply | Not implemented | — |
| Advanced Filter | Not implemented | No criteria range, copy to another location or unique records |
| Sort by custom list | Partial | Can be set and written to OOXML; not applied by the local sort |
| Sort left to right | Partial | As above |
| Sort by color | Partial | A single color can be placed on top; an order of several colors is not supported, and a DXF style index must be entered |
| Sorting ranges that contain merged cells | Not implemented | Rejected by the service |

### 4.2 Data cleanup and analysis

| Excel feature | Status | Notes |
| --- | --- | --- |
| Text to Columns | Not implemented | — |
| Remove Duplicates | Not implemented | — |
| Consolidate | Preserved only | The `dataConsolidate` element is retained |
| Subtotal | Not implemented | The `SUBTOTAL` function is available |
| Outline (Group, Ungroup) | Not implemented | `outlineLevel` is neither read nor written |
| Forecast Sheet | Not implemented | The `FORECAST` function family is available |
| Solver | Not implemented | Goal Seek is available |
| Scenario summary report | Not implemented | Scenario Manager is available |
| Data Table | Partial | Results are written as values rather than a `TABLE()` array formula and do not update when inputs change |
| Linked data types (Stocks, Geography) | Not implemented | — |
| Circle Invalid Data | Not implemented | Data validation with 8 types, drop-down lists, input messages and error alerts is available |

### 4.3 Get & Transform

| Excel feature | Status | Notes |
| --- | --- | --- |
| Get Data (files, databases, web and other sources) | Not implemented | Connections and queries cannot be created |
| Existing connections and query tables | Existing objects only | Name, refresh policy, `commandText` and similar properties can be changed; items cannot be created or deleted |
| Refresh All | Not implemented | Connections and query tables have no refresh action; only refresh-related properties can be changed |
| Power Query Editor | Partial | An offline evaluator for a subset of M (some twenty functions) accepts manually supplied JSON or CSV; it does not read queries stored in the workbook, and results cannot be written to a worksheet |
| Data Model, relationships, DAX | Preserved only | Mashup and VertiPaq parts are retained byte-for-byte |

## 5. Tables, PivotTables and slicers

| Excel feature | Status | Notes |
| --- | --- | --- |
| Create table | Partial | Created from the selection; no Ctrl+T, no "My table has headers" option, and default column headers are not written |
| Resize table | Partial | By typing a range address; no drag handle |
| Create PivotTable | Not implemented | `/api/pivot-tables` operations are `list`, `update` and `reset` |
| Drag fields between PivotTable areas | Not implemented | Areas are changed with a drop-down and move up/down buttons |
| Aggregations in local refresh | Partial | Sum, Count, Average, Max, Min and Distinct Count; other summary functions are written to OOXML but not calculated locally |
| Show Values As | Existing objects only | 9 options can be written and are not calculated by local refresh; the 6 options added later, such as % of Parent Total and Rank, are absent |
| Calculated fields and calculated items | Not implemented | — |
| Numeric and manual grouping | Not implemented | Date grouping is available |
| Sort by value field, Top N filter | Not implemented | — |
| Data Model, OLAP, multiple consolidation ranges | Not implemented | Local refresh rejects these sources |
| PivotChart | Not implemented | — |
| Recommended PivotTables | Not implemented | — |
| Create slicer | Partial | An existing slicer can be duplicated; a slicer cannot be created for a PivotTable or table that has none |
| Slicer filtering | Partial | Changing the selected items does not trigger PivotTable recalculation |
| Create timeline | Not implemented | Static date ranges of existing timelines can be edited; relative period filters are preserved read-only |
| Slicer and timeline style galleries, position and size | Not implemented | Styles are entered as text |

## 6. Charts and graphic objects

### 6.1 Charts

| Excel feature | Status | Notes |
| --- | --- | --- |
| Create chart | Not implemented | The Insert tab has no chart entry; the service can only duplicate an existing chart part |
| Column, bar, line, pie, doughnut, area, scatter, bubble, radar, stock, combo | Existing objects only | Title, legend, axes, data labels, trendlines, error bars, secondary axis and data series can be edited |
| Change chart type | Partial | Within the same type family only; stock charts cannot be converted to or from other types |
| Waterfall, funnel, histogram, Pareto, box and whisker, sunburst, treemap, map | Preserved only | `chartex` parts cannot be read or edited |
| Surface and 3-D charts | Preserved only | — |
| Recommended Charts, Quick Layout, Chart Styles, Change Colors | Not implemented | — |
| Fill, border and font of the chart area and plot area; gridline styles | Not implemented | Gridlines have an on/off switch only |
| Data table, up/down bars, high-low lines | Preserved only | — |
| Switch Row/Column, Chart Filters | Not implemented | — |
| Export chart as picture | Not implemented | The service has no rasterizer |
| Sparklines | Not implemented | No related code in the repository |

### 6.2 Shapes, pictures and SmartArt

| Excel feature | Status | Notes |
| --- | --- | --- |
| Create shape | Not implemented | An existing shape can only be duplicated |
| Preset shapes | Partial | The interface offers 7; ECMA-376 defines 187, and other imported values are retained as they are |
| Picture, texture and pattern fills | Preserved only | Solid and gradient fills can be edited |
| Glow, reflection, bevel and 3-D effects | Preserved only | Outer shadow and soft edges can be edited |
| Line arrows, cap and join types | Preserved only | — |
| Connector snapping, elbow and curved connectors | Not implemented | — |
| Bring Forward / Send Backward, Align and Distribute, Group and Ungroup, Selection Pane | Not implemented | — |
| Alt text, lock aspect ratio | Not implemented | — |
| Move and size with cells | Not implemented | Move with cells and fixed position are available |
| Picture crop, corrections, artistic effects, remove background, compress | Not implemented | Imported picture objects cannot be edited |
| Icons, WordArt, 3D models, screenshots, online pictures | Not implemented | — |
| Create SmartArt | Not implemented | — |
| SmartArt layouts, styles and colors | Preserved only | Node text and hierarchy can be edited |
| Cell hyperlinks | Not implemented | The Link command on the Insert tab creates a floating text object containing a link and does not write worksheet `hyperlinks`; hyperlinks in imported files are retained |
| Symbols | Not implemented | — |
| Equation objects | Partial | Entered and rendered as LaTeX and converted to OMML on export; no equation editor or template gallery |

Text boxes, pictures, SVG, video and HTML objects created in the interface belong to the UniCell object model and are converted to bitmaps when exported to XLSX.

## 7. Page layout and printing

| Excel feature | Status | Notes |
| --- | --- | --- |
| Page Layout tab | Not implemented | Page setup is in the Page and Review dialog on the View tab |
| Document themes (colors, fonts, effects) | Preserved only | `theme1.xml` is retained; the color palette uses fixed default theme colors |
| Sheet background | Not implemented | — |
| Paper size | Partial | The interface offers 10 sizes; other imported values are retained |
| Margin presets, unit switching | Not implemented | The 6 margins are entered in inches |
| Print area | Partial | Entered as a defined-name formula in text; no "set to current selection" command and no syntax validation |
| Print titles | Partial | Rows to repeat at top and columns to repeat at left share one text box |
| Page breaks | Partial | Can be added, removed and changed; no "insert at the active cell" and no Reset All Page Breaks |
| Headers and footers | Partial | Entered as text with codes such as `&P` and `&N`; no insert buttons, built-in presets or formatting; the header picture code `&G` is not rendered |
| Page Break Preview | Partial | Draws guide lines at a fixed A4 size and does not read page setup, the print area or manual page breaks |
| Page Layout view, ruler | Not implemented | — |
| Printer selection, duplex printing | Not implemented | Handled by the browser print dialog |

## 8. Review and protection

| Excel feature | Status | Notes |
| --- | --- | --- |
| Review tab | Not implemented | Comments and protection are in the Page and Review dialog |
| Spelling, Thesaurus, Translate, Check Accessibility | Not implemented | — |
| Comments shown on the grid | Not implemented | Notes and comments are listed and edited in a dialog only; no Show All, Previous or Next |
| Note box formatting | Preserved only | VML shape properties are retained |
| Threaded comments | Partial | Create, reply, delete and mention; the resolved state cannot be viewed or changed, replies to replies are not supported, and mention positions are entered manually |
| Track Changes, shared workbooks, Compare and Merge | Not implemented | — |
| Cell Locked and Hidden properties | Not implemented | No entry for setting them; sheet protection permissions are available |
| User permissions for Allow Edit Ranges | Partial | Ranges and passwords can be edited; `securityDescriptor` is entered as text |
| File encryption | Not implemented | See section 1 |

## 9. View and window

| Excel feature | Status | Notes |
| --- | --- | --- |
| Freeze Top Row, Freeze First Column | Partial | Only freezing at the active cell is offered |
| Split | Not implemented | — |
| New Window, Arrange All, View Side by Side, Synchronous Scrolling | Not implemented | — |
| Custom Views | Preserved only | `customSheetViews` is retained |
| Headings (row and column headers) toggle | Not implemented | Gridline and formula bar toggles are available |
| Zoom to Selection, Zoom dialog | Not implemented | Zoom buttons, a slider and Ctrl+wheel are available |
| Full-screen worksheet | Not implemented | — |
| Status bar statistics | Partial | Sum, Count and Average; no Max, Min, Numerical Count or customization |

## 10. Automation and extensibility

| Excel feature | Status | Notes |
| --- | --- | --- |
| VBA macro execution | Preserved only | `vbaProject.bin` is retained and not executed |
| Macro recording, Visual Basic Editor | Not implemented | — |
| Office Scripts | Not implemented | — |
| Add-ins (Office Add-ins, XLL, COM) | Not implemented | — |
| Form controls, ActiveX controls | Preserved only | — |
| Custom functions | Partial | A LAMBDA can be stored in a defined name; add-in custom functions are not available |

## 11. Interaction, internationalization and accessibility

| Excel feature | Status | Notes |
| --- | --- | --- |
| Common shortcuts | Partial | Not bound: Ctrl+1, Ctrl+D, Ctrl+R, Ctrl+;, Ctrl+K, Ctrl+T, Ctrl+Shift+L, Ctrl+9, Ctrl+0, Ctrl+PageUp/PageDown, Ctrl+End, Ctrl+Space, Shift+Space, Ctrl+`, F5, Shift+F11 |
| Alt KeyTips | Not implemented | — |
| Shortcut menu | Partial | No New Comment, Filter, Sort, Define Name, Link or Pick From Drop-down List |
| Quick Analysis | Not implemented | — |
| Interface language | Not implemented | Interface text is fixed in Simplified Chinese; there is no localization mechanism |
| Accessibility | Partial | The ribbon and dialogs have ARIA semantics and focus management; the grid has no `role="grid"` or cell semantics |
| Touch | Not implemented | Grid interaction is based on mouse events; no touch gestures or pinch zoom |

## 12. Outside the product scope

The [project overview](README.md) states that the following are not part of the public local edition, so they are not itemized here: cloud storage, shared links, collaborative editing, centralized accounts and hosted quotas. The related Excel capabilities (co-authoring, OneDrive and SharePoint integration, Microsoft 365 Copilot, Python in Excel, Power BI integration) are likewise out of scope.

## Maintenance

This list corresponds to the baseline commit above. When an implementation or boundary changes, update the affected entries and the baseline. New entries should carry a status and cite a searchable identifier (endpoint path, constant, function or file name) as evidence.
