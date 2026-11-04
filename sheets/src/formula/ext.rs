//! Additional worksheet functions (dynamic arrays, statistics, engineering, finance).

use super::eval::{Evaluator, Res};
use super::parser::Expr;
use crate::model::ErrorKind as E;

pub fn call(_ev: &mut Evaluator, _name: &str, _a: &[Expr]) -> Res {
    Err(E::Name)
}
