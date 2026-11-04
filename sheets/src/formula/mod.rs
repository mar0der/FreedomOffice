//! Formula engine: parsing, evaluation and the function library.

mod deps;
mod eval;
mod ext;
mod functions;
mod parser;
#[cfg(test)]
mod bench;
#[cfg(test)]
mod tests;

pub use deps::CalcState;
pub use eval::{compare, eval_expr, eval_formula, eval_grid, recalc, recalc_ranges};
#[cfg(test)]
pub use eval::{eval_array, recalc_cells};
pub use functions::{function_names, signature};
pub use parser::{
    lex, map_refs, move_refs, parse, parse_cell_part, print, print_xlsx, quote_sheet, ref_str, rename_sheet, shift_for_structure,
    shift_relative, Expr, RefExpr, RefKind, Spanned, Tok,
};
