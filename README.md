# FreedomOffice Sheets

A lightweight, open-source spreadsheet for Windows, written in Rust.
One portable ~9 MB executable — no installer, no runtime, no account.

It deliberately follows the familiar desktop-spreadsheet layout and keyboard habits
(ribbon, formula bar, Alt key sequences, Ctrl/Shift/F-key shortcuts), so years of
muscle memory keep working. It reads and writes `.xlsx`, so your existing workbooks open as they are.

![FreedomOffice Sheets](assets/screenshot.png)

## Download

Grab `FreedomSheets.exe` from the [latest release](https://github.com/mar0der/FreedomOffice/releases/latest)
and run it. To open a file, use **File → Open**, `Ctrl+O`, or drag a workbook onto the exe.

## Features

**Interface**
- Ribbon with Home, Insert, Page Layout, Formulas, Data, Review, View and Help tabs, plus a contextual Chart Design tab
- File backstage (New, Open, Save As, Print, Recent files), Quick Access Toolbar, name box, formula bar
- Sheet tabs (add, rename, colour, hide, drag to reorder), status bar with Sum / Count / Average and zoom slider
- Alt KeyTips: `Alt, H, O, I` (AutoFit column), `Alt, H, B, A` (all borders), `Alt, W, F, F` (freeze panes) and many more

**Formulas**
- 287 functions: math, statistics, text, date/time, logical, lookup (VLOOKUP, INDEX/MATCH, XLOOKUP, XMATCH), financial, engineering, LET
- Dynamic arrays that spill (`FILTER`, `SORT`, `UNIQUE`, `SEQUENCE`, `TEXTSPLIT`, `VSTACK`, … and the `A1#` spill reference)
- Fast incremental recalculation (125k formulas recalculate in ~0.2 s; single edits in under a millisecond)
- Point-and-click references with colour-coded highlights, function autocomplete, argument hints, `F4` to toggle `$`
- Named ranges, cross-sheet references, circular-reference detection

**Editing and formatting**
- Fonts, fills, borders, alignment, text rotation, wrap text (rows grow to fit), merge & center, indent
- Number formats (General, Number, Currency, Accounting, Date, Time, Percentage, Scientific, Text, custom codes)
- Cell styles, Format as Table, Format Painter (double-click to keep it on)
- Conditional formatting: highlight rules, top/bottom, above/below average, duplicates, formulas, data bars, colour scales
- Data validation with in-cell dropdown lists, input messages and error alerts
- Fill handle with series (numbers, dates, months, weekdays, “Item 1”…), Ctrl+D / Ctrl+R, AutoComplete from the column
- Drag-to-move cells, Ctrl+click multi-selection, F4 / Ctrl+Y repeat last action, cell notes, hyperlinks
- Undo / redo, AutoRecover (unsaved work is offered again after an unexpected exit)

**Data**
- Sort (multi-level), AutoFilter, remove duplicates, text to columns
- Group / ungroup rows and columns with outline buttons, freeze panes, hide/unhide rows, columns and sheets
- Find & Replace, Go To, select precedents / dependents (`Ctrl+[` / `Ctrl+]`)

**Charts**
- Column, bar, line, area, pie, doughnut and scatter charts that update live, with title, legend and data labels

**Files**

| Format | Open | Save |
|---|---|---|
| Excel workbook `.xlsx` / `.xlsm` | ✓ (values, formulas, styles, merges, charts, conditional formats, validation, notes, links, groups) | ✓ |
| Excel 97-2003 `.xls`, binary `.xlsb` | ✓ (values and formulas) | |
| OpenDocument `.ods` | ✓ (values and formulas) | |
| CSV / TSV / text | ✓ (delimiter and encoding detection) | ✓ |
| PDF | | ✓ (File → Print / Export to PDF) |

## Keyboard shortcuts (selection)

| Keys | Action |
|---|---|
| `F2` · `Esc` · `Alt+Enter` | Edit cell · cancel · new line in cell |
| `Ctrl+Arrow` · `Ctrl+Shift+Arrow` | Jump / extend to the edge of the data |
| `Ctrl+1` | Format Cells |
| `Ctrl+B / I / U / 5` | Bold / italic / underline / strikethrough |
| `Ctrl+Shift+$ % # @ !` | Currency / percent / date / time / number format |
| `Alt+=` · `Shift+F3` | AutoSum · Insert Function |
| `Ctrl+D` · `Ctrl+R` · `Ctrl+Enter` | Fill down · fill right · fill selection |
| `Ctrl+;` · `Ctrl+Shift+;` | Insert date · time |
| `Ctrl+Space` · `Shift+Space` · `Ctrl+A` | Select column · row · region/all |
| `Ctrl+-` · `Ctrl++` | Delete / insert cells |
| `Ctrl+Shift+L` · `Ctrl+T` | Toggle filter · format as table |
| `Alt+Shift+→` / `←` | Group / ungroup |
| `Ctrl+K` · `Shift+F2` | Hyperlink · note |
| `Ctrl+PgUp` / `PgDn` · `Shift+F11` | Previous / next sheet · new sheet |
| `Ctrl+F` / `H` / `G` | Find / replace / go to |
| `Ctrl+S` · `F12` · `Ctrl+P` | Save · save as · print |

The full list is under **Help → Keyboard Shortcuts**.

## Building from source

Requires Rust 1.88+ and the MSVC toolchain on Windows.

```bash
cargo build --release
```

The executable is written to `target/release/FreedomSheets.exe` (statically linked C runtime, icon and DPI-aware manifest embedded).
Run the tests with `cargo test`.

## Project layout

```
sheets/src/
  model.rs        workbook, sheets, cells, styles
  numfmt.rs       number format codes, dates, input parsing
  formula/        parser, evaluator, dependency graph, function library
  io/             xlsx reader/writer, xls/xlsb/ods (calamine), csv
  chart.rs, condfmt.rs, validation.rs, pdf.rs
  app/            UI (egui): grid, ribbon, dialogs, keyboard, KeyTips, charts
```

## Not yet supported

Pivot tables, macros/VBA, images and shapes, sparklines, threaded comments, sheet protection,
icon sets, localized function names and right-to-left sheets.

## License

[MIT](LICENSE). This is an independent, clean-room project. It is not affiliated with or endorsed by
Microsoft; Microsoft Excel is a trademark of Microsoft Corporation and is mentioned only to describe file compatibility.
