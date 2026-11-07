//! FreedomOffice Sheets as a library: the workbook model, the formula engine, file import and
//! export, and the live co-editing state, without any user interface. The egui application
//! (`app`, behind the `ui` feature through the `FreedomSheets` binary) and the headless
//! `fsheets` agent client both build on it.

pub mod chart;
pub mod cloud;
pub mod condfmt;
pub mod datatools;
pub mod filter;
pub mod formula;
pub mod hash;
pub mod iconset;
pub mod io;
pub mod live;
pub mod model;
pub mod numfmt;
pub mod page;
pub mod pivot;
pub mod print;
pub mod solver;
pub mod sparkline;
pub mod structure;
pub mod table;
pub mod validation;
pub mod whatif;
