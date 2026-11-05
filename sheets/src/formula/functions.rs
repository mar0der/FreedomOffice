//! Built-in worksheet functions.

use std::cmp::Ordering;

use std::rc::Rc;

use super::eval::{boolean, compare, num, text, to_bool, to_num, to_text, values_equal, Evaluator, LookupCol, Res, EV, MAX_TEXT};
use super::parser::{Expr, RefKind};
use crate::model::{ErrorKind as E, Range, Value, MAX_COLS, MAX_ROWS};
use crate::numfmt;

/// (name, arguments, category)
pub const FUNCS: &[(&str, &str, &str)] = &[
    ("ABS", "number", "Math & Trig"),
    ("ACOS", "number", "Math & Trig"),
    ("ACOSH", "number", "Math & Trig"),
    ("ACOT", "number", "Math & Trig"),
    ("ACOTH", "number", "Math & Trig"),
    ("ADDRESS", "row_num, column_num, [abs_num], [a1], [sheet_text]", "Lookup & Reference"),
    ("AGGREGATE", "function_num, options, ref1, [ref2], ...", "Math & Trig"),
    ("AND", "logical1, [logical2], ...", "Logical"),
    ("ARABIC", "text", "Math & Trig"),
    ("AREAS", "reference", "Lookup & Reference"),
    ("ARRAYTOTEXT", "array, [format]", "Text"),
    ("ASIN", "number", "Math & Trig"),
    ("ASINH", "number", "Math & Trig"),
    ("ATAN", "number", "Math & Trig"),
    ("ATAN2", "x_num, y_num", "Math & Trig"),
    ("ATANH", "number", "Math & Trig"),
    ("AVEDEV", "number1, [number2], ...", "Statistical"),
    ("AVERAGE", "number1, [number2], ...", "Statistical"),
    ("AVERAGEA", "value1, [value2], ...", "Statistical"),
    ("AVERAGEIF", "range, criteria, [average_range]", "Statistical"),
    ("AVERAGEIFS", "average_range, criteria_range1, criteria1, ...", "Statistical"),
    ("BASE", "number, radix, [min_length]", "Math & Trig"),
    ("BETA.DIST", "x, alpha, beta, cumulative, [A], [B]", "Statistical"),
    ("BETA.INV", "probability, alpha, beta, [A], [B]", "Statistical"),
    ("BETADIST", "x, alpha, beta, [A], [B]", "Statistical"),
    ("BETAINV", "probability, alpha, beta, [A], [B]", "Statistical"),
    ("BIN2DEC", "number", "Engineering"),
    ("BIN2HEX", "number, [places]", "Engineering"),
    ("BIN2OCT", "number, [places]", "Engineering"),
    ("BINOM.DIST", "number_s, trials, probability_s, cumulative", "Statistical"),
    ("BINOM.DIST.RANGE", "trials, probability_s, number_s, [number_s2]", "Statistical"),
    ("BINOM.INV", "trials, probability_s, alpha", "Statistical"),
    ("BINOMDIST", "number_s, trials, probability_s, cumulative", "Statistical"),
    ("BITAND", "number1, number2", "Engineering"),
    ("BITLSHIFT", "number, shift_amount", "Engineering"),
    ("BITOR", "number1, number2", "Engineering"),
    ("BITRSHIFT", "number, shift_amount", "Engineering"),
    ("BITXOR", "number1, number2", "Engineering"),
    ("BYCOL", "array, lambda(column)", "Logical"),
    ("BYROW", "array, lambda(row)", "Logical"),
    ("CEILING", "number, significance", "Math & Trig"),
    ("CEILING.MATH", "number, [significance], [mode]", "Math & Trig"),
    ("CEILING.PRECISE", "number, [significance]", "Math & Trig"),
    ("CELL", "info_type, [reference]", "Information"),
    ("CHAR", "number", "Text"),
    ("CHIDIST", "x, deg_freedom", "Statistical"),
    ("CHIINV", "probability, deg_freedom", "Statistical"),
    ("CHISQ.DIST", "x, deg_freedom, cumulative", "Statistical"),
    ("CHISQ.DIST.RT", "x, deg_freedom", "Statistical"),
    ("CHISQ.INV", "probability, deg_freedom", "Statistical"),
    ("CHISQ.INV.RT", "probability, deg_freedom", "Statistical"),
    ("CHISQ.TEST", "actual_range, expected_range", "Statistical"),
    ("CHITEST", "actual_range, expected_range", "Statistical"),
    ("CHOOSE", "index_num, value1, [value2], ...", "Lookup & Reference"),
    ("CHOOSECOLS", "array, col_num1, [col_num2], ...", "Lookup & Reference"),
    ("CHOOSEROWS", "array, row_num1, [row_num2], ...", "Lookup & Reference"),
    ("CLEAN", "text", "Text"),
    ("CODE", "text", "Text"),
    ("COLUMN", "[reference]", "Lookup & Reference"),
    ("COLUMNS", "array", "Lookup & Reference"),
    ("COMBIN", "number, number_chosen", "Math & Trig"),
    ("COMBINA", "number, number_chosen", "Math & Trig"),
    ("CONCAT", "text1, [text2], ...", "Text"),
    ("CONCATENATE", "text1, [text2], ...", "Text"),
    ("CONFIDENCE", "alpha, standard_dev, size", "Statistical"),
    ("CONFIDENCE.NORM", "alpha, standard_dev, size", "Statistical"),
    ("CONFIDENCE.T", "alpha, standard_dev, size", "Statistical"),
    ("CONVERT", "number, from_unit, to_unit", "Engineering"),
    ("CORREL", "array1, array2", "Statistical"),
    ("COS", "number", "Math & Trig"),
    ("COSH", "number", "Math & Trig"),
    ("COT", "number", "Math & Trig"),
    ("COTH", "number", "Math & Trig"),
    ("COUNT", "value1, [value2], ...", "Statistical"),
    ("COUNTA", "value1, [value2], ...", "Statistical"),
    ("COUNTBLANK", "range", "Statistical"),
    ("COUNTIF", "range, criteria", "Statistical"),
    ("COUNTIFS", "criteria_range1, criteria1, ...", "Statistical"),
    ("COVAR", "array1, array2", "Statistical"),
    ("COVARIANCE.P", "array1, array2", "Statistical"),
    ("COVARIANCE.S", "array1, array2", "Statistical"),
    ("CRITBINOM", "trials, probability_s, alpha", "Statistical"),
    ("CSC", "number", "Math & Trig"),
    ("CSCH", "number", "Math & Trig"),
    ("CUMIPMT", "rate, nper, pv, start_period, end_period, type", "Financial"),
    ("CUMPRINC", "rate, nper, pv, start_period, end_period, type", "Financial"),
    ("DATE", "year, month, day", "Date & Time"),
    ("DATEDIF", "start_date, end_date, unit", "Date & Time"),
    ("DATEVALUE", "date_text", "Date & Time"),
    ("DAVERAGE", "database, field, criteria", "Database"),
    ("DAY", "serial_number", "Date & Time"),
    ("DAYS", "end_date, start_date", "Date & Time"),
    ("DAYS360", "start_date, end_date, [method]", "Date & Time"),
    ("DB", "cost, salvage, life, period, [month]", "Financial"),
    ("DCOUNT", "database, [field], criteria", "Database"),
    ("DCOUNTA", "database, [field], criteria", "Database"),
    ("DDB", "cost, salvage, life, period, [factor]", "Financial"),
    ("DEC2BIN", "number, [places]", "Engineering"),
    ("DEC2HEX", "number, [places]", "Engineering"),
    ("DEC2OCT", "number, [places]", "Engineering"),
    ("DECIMAL", "text, radix", "Math & Trig"),
    ("DEGREES", "angle", "Math & Trig"),
    ("DELTA", "number1, [number2]", "Engineering"),
    ("DEVSQ", "number1, [number2], ...", "Statistical"),
    ("DGET", "database, field, criteria", "Database"),
    ("DMAX", "database, field, criteria", "Database"),
    ("DMIN", "database, field, criteria", "Database"),
    ("DOLLAR", "number, [decimals]", "Text"),
    ("DOLLARDE", "fractional_dollar, fraction", "Financial"),
    ("DOLLARFR", "decimal_dollar, fraction", "Financial"),
    ("DPRODUCT", "database, field, criteria", "Database"),
    ("DROP", "array, rows, [columns]", "Lookup & Reference"),
    ("DSTDEV", "database, field, criteria", "Database"),
    ("DSTDEVP", "database, field, criteria", "Database"),
    ("DSUM", "database, field, criteria", "Database"),
    ("DVAR", "database, field, criteria", "Database"),
    ("DVARP", "database, field, criteria", "Database"),
    ("EDATE", "start_date, months", "Date & Time"),
    ("EFFECT", "nominal_rate, npery", "Financial"),
    ("ENCODEURL", "text", "Web"),
    ("EOMONTH", "start_date, months", "Date & Time"),
    ("ERF", "lower_limit, [upper_limit]", "Engineering"),
    ("ERF.PRECISE", "x", "Engineering"),
    ("ERFC", "x", "Engineering"),
    ("ERFC.PRECISE", "x", "Engineering"),
    ("ERROR.TYPE", "error_val", "Information"),
    ("EVEN", "number", "Math & Trig"),
    ("EXACT", "text1, text2", "Text"),
    ("EXP", "number", "Math & Trig"),
    ("EXPAND", "array, rows, [columns], [pad_with]", "Lookup & Reference"),
    ("EXPON.DIST", "x, lambda, cumulative", "Statistical"),
    ("EXPONDIST", "x, lambda, cumulative", "Statistical"),
    ("F.DIST", "x, deg_freedom1, deg_freedom2, cumulative", "Statistical"),
    ("F.DIST.RT", "x, deg_freedom1, deg_freedom2", "Statistical"),
    ("F.INV", "probability, deg_freedom1, deg_freedom2", "Statistical"),
    ("F.INV.RT", "probability, deg_freedom1, deg_freedom2", "Statistical"),
    ("F.TEST", "array1, array2", "Statistical"),
    ("FACT", "number", "Math & Trig"),
    ("FACTDOUBLE", "number", "Math & Trig"),
    ("FALSE", "", "Logical"),
    ("FDIST", "x, deg_freedom1, deg_freedom2", "Statistical"),
    ("FILTER", "array, include, [if_empty]", "Lookup & Reference"),
    ("FIND", "find_text, within_text, [start_num]", "Text"),
    ("FINDB", "find_text, within_text, [start_num]", "Text"),
    ("FINV", "probability, deg_freedom1, deg_freedom2", "Statistical"),
    ("FISHER", "x", "Statistical"),
    ("FISHERINV", "y", "Statistical"),
    ("FIXED", "number, [decimals], [no_commas]", "Text"),
    ("FLOOR", "number, significance", "Math & Trig"),
    ("FLOOR.MATH", "number, [significance], [mode]", "Math & Trig"),
    ("FLOOR.PRECISE", "number, [significance]", "Math & Trig"),
    ("FORECAST", "x, known_y's, known_x's", "Statistical"),
    ("FORECAST.LINEAR", "x, known_y's, known_x's", "Statistical"),
    ("FORMULATEXT", "reference", "Lookup & Reference"),
    ("FREQUENCY", "data_array, bins_array", "Statistical"),
    ("FTEST", "array1, array2", "Statistical"),
    ("FV", "rate, nper, pmt, [pv], [type]", "Financial"),
    ("FVSCHEDULE", "principal, schedule", "Financial"),
    ("GAMMA", "x", "Statistical"),
    ("GAMMA.DIST", "x, alpha, beta, cumulative", "Statistical"),
    ("GAMMA.INV", "probability, alpha, beta", "Statistical"),
    ("GAMMADIST", "x, alpha, beta, cumulative", "Statistical"),
    ("GAMMAINV", "probability, alpha, beta", "Statistical"),
    ("GAMMALN", "x", "Statistical"),
    ("GAMMALN.PRECISE", "x", "Statistical"),
    ("GAUSS", "z", "Statistical"),
    ("GCD", "number1, [number2], ...", "Math & Trig"),
    ("GEOMEAN", "number1, [number2], ...", "Statistical"),
    ("GESTEP", "number, [step]", "Engineering"),
    ("GETPIVOTDATA", "data_field, pivot_table, [field1, item1], ...", "Lookup & Reference"),
    ("GROUPBY", "row_fields, values, function, [field_headers], [total_depth], [sort_order], [filter_array]", "Lookup & Reference"),
    ("GROWTH", "known_y's, [known_x's], [new_x's], [const]", "Statistical"),
    ("HARMEAN", "number1, [number2], ...", "Statistical"),
    ("HEX2BIN", "number, [places]", "Engineering"),
    ("HEX2DEC", "number", "Engineering"),
    ("HEX2OCT", "number, [places]", "Engineering"),
    ("HLOOKUP", "lookup_value, table_array, row_index_num, [range_lookup]", "Lookup & Reference"),
    ("HOUR", "serial_number", "Date & Time"),
    ("HSTACK", "array1, [array2], ...", "Lookup & Reference"),
    ("HYPERLINK", "link_location, [friendly_name]", "Lookup & Reference"),
    ("HYPGEOM.DIST", "sample_s, number_sample, population_s, number_pop, cumulative", "Statistical"),
    ("HYPGEOMDIST", "sample_s, number_sample, population_s, number_pop", "Statistical"),
    ("IF", "logical_test, [value_if_true], [value_if_false]", "Logical"),
    ("IFERROR", "value, value_if_error", "Logical"),
    ("IFNA", "value, value_if_na", "Logical"),
    ("IFS", "logical_test1, value_if_true1, ...", "Logical"),
    ("INDEX", "array, row_num, [column_num]", "Lookup & Reference"),
    ("INDIRECT", "ref_text, [a1]", "Lookup & Reference"),
    ("INFO", "type_text", "Information"),
    ("INT", "number", "Math & Trig"),
    ("INTERCEPT", "known_y's, known_x's", "Statistical"),
    ("IPMT", "rate, per, nper, pv, [fv], [type]", "Financial"),
    ("IRR", "values, [guess]", "Financial"),
    ("ISBLANK", "value", "Information"),
    ("ISERR", "value", "Information"),
    ("ISERROR", "value", "Information"),
    ("ISEVEN", "number", "Information"),
    ("ISFORMULA", "reference", "Information"),
    ("ISLOGICAL", "value", "Information"),
    ("ISNA", "value", "Information"),
    ("ISNONTEXT", "value", "Information"),
    ("ISNUMBER", "value", "Information"),
    ("ISO.CEILING", "number, [significance]", "Math & Trig"),
    ("ISODD", "number", "Information"),
    ("ISOMITTED", "argument", "Information"),
    ("ISOWEEKNUM", "date", "Date & Time"),
    ("ISPMT", "rate, per, nper, pv", "Financial"),
    ("ISREF", "value", "Information"),
    ("ISTEXT", "value", "Information"),
    ("KURT", "number1, [number2], ...", "Statistical"),
    ("LAMBDA", "[parameter1, ...], calculation", "Logical"),
    ("LARGE", "array, k", "Statistical"),
    ("LCM", "number1, [number2], ...", "Math & Trig"),
    ("LEFT", "text, [num_chars]", "Text"),
    ("LEFTB", "text, [num_bytes]", "Text"),
    ("LEN", "text", "Text"),
    ("LENB", "text", "Text"),
    ("LET", "name1, name_value1, calculation_or_name2, ...", "Logical"),
    ("LINEST", "known_y's, [known_x's], [const], [stats]", "Statistical"),
    ("LN", "number", "Math & Trig"),
    ("LOG", "number, [base]", "Math & Trig"),
    ("LOG10", "number", "Math & Trig"),
    ("LOGINV", "probability, mean, standard_dev", "Statistical"),
    ("LOGNORM.DIST", "x, mean, standard_dev, cumulative", "Statistical"),
    ("LOGNORM.INV", "probability, mean, standard_dev", "Statistical"),
    ("LOGNORMDIST", "x, mean, standard_dev", "Statistical"),
    ("LOOKUP", "lookup_value, lookup_vector, [result_vector]", "Lookup & Reference"),
    ("LOWER", "text", "Text"),
    ("MAKEARRAY", "rows, columns, lambda(row, column)", "Logical"),
    ("MAP", "array1, [array2], ..., lambda", "Logical"),
    ("MATCH", "lookup_value, lookup_array, [match_type]", "Lookup & Reference"),
    ("MAX", "number1, [number2], ...", "Statistical"),
    ("MAXA", "value1, [value2], ...", "Statistical"),
    ("MAXIFS", "max_range, criteria_range1, criteria1, ...", "Statistical"),
    ("MDETERM", "array", "Math & Trig"),
    ("MEDIAN", "number1, [number2], ...", "Statistical"),
    ("MID", "text, start_num, num_chars", "Text"),
    ("MIDB", "text, start_num, num_bytes", "Text"),
    ("MIN", "number1, [number2], ...", "Statistical"),
    ("MINA", "value1, [value2], ...", "Statistical"),
    ("MINIFS", "min_range, criteria_range1, criteria1, ...", "Statistical"),
    ("MINUTE", "serial_number", "Date & Time"),
    ("MINVERSE", "array", "Math & Trig"),
    ("MIRR", "values, finance_rate, reinvest_rate", "Financial"),
    ("MMULT", "array1, array2", "Math & Trig"),
    ("MOD", "number, divisor", "Math & Trig"),
    ("MODE", "number1, [number2], ...", "Statistical"),
    ("MODE.MULT", "number1, [number2], ...", "Statistical"),
    ("MODE.SNGL", "number1, [number2], ...", "Statistical"),
    ("MONTH", "serial_number", "Date & Time"),
    ("MROUND", "number, multiple", "Math & Trig"),
    ("MULTINOMIAL", "number1, [number2], ...", "Math & Trig"),
    ("MUNIT", "dimension", "Math & Trig"),
    ("N", "value", "Information"),
    ("NA", "", "Information"),
    ("NEGBINOM.DIST", "number_f, number_s, probability_s, cumulative", "Statistical"),
    ("NEGBINOMDIST", "number_f, number_s, probability_s", "Statistical"),
    ("NETWORKDAYS", "start_date, end_date, [holidays]", "Date & Time"),
    ("NETWORKDAYS.INTL", "start_date, end_date, [weekend], [holidays]", "Date & Time"),
    ("NOMINAL", "effect_rate, npery", "Financial"),
    ("NORM.DIST", "x, mean, standard_dev, cumulative", "Statistical"),
    ("NORM.INV", "probability, mean, standard_dev", "Statistical"),
    ("NORM.S.DIST", "z, cumulative", "Statistical"),
    ("NORM.S.INV", "probability", "Statistical"),
    ("NORMDIST", "x, mean, standard_dev, cumulative", "Statistical"),
    ("NORMINV", "probability, mean, standard_dev", "Statistical"),
    ("NORMSDIST", "z", "Statistical"),
    ("NORMSINV", "probability", "Statistical"),
    ("NOT", "logical", "Logical"),
    ("NOW", "", "Date & Time"),
    ("NPER", "rate, pmt, pv, [fv], [type]", "Financial"),
    ("NPV", "rate, value1, [value2], ...", "Financial"),
    ("NUMBERVALUE", "text, [decimal_separator], [group_separator]", "Text"),
    ("OCT2BIN", "number, [places]", "Engineering"),
    ("OCT2DEC", "number", "Engineering"),
    ("OCT2HEX", "number, [places]", "Engineering"),
    ("ODD", "number", "Math & Trig"),
    ("OFFSET", "reference, rows, cols, [height], [width]", "Lookup & Reference"),
    ("OR", "logical1, [logical2], ...", "Logical"),
    ("PDURATION", "rate, pv, fv", "Financial"),
    ("PEARSON", "array1, array2", "Statistical"),
    ("PERCENTILE", "array, k", "Statistical"),
    ("PERCENTILE.EXC", "array, k", "Statistical"),
    ("PERCENTILE.INC", "array, k", "Statistical"),
    ("PERCENTOF", "data_subset, data_all", "Math & Trig"),
    ("PERCENTRANK", "array, x, [significance]", "Statistical"),
    ("PERCENTRANK.EXC", "array, x, [significance]", "Statistical"),
    ("PERCENTRANK.INC", "array, x, [significance]", "Statistical"),
    ("PERMUT", "number, number_chosen", "Statistical"),
    ("PERMUTATIONA", "number, number_chosen", "Statistical"),
    ("PHI", "x", "Statistical"),
    ("PI", "", "Math & Trig"),
    ("PIVOTBY", "row_fields, col_fields, values, function, [field_headers], [row_total_depth], [row_sort_order], [col_total_depth], [col_sort_order], [filter_array]", "Lookup & Reference"),
    ("PMT", "rate, nper, pv, [fv], [type]", "Financial"),
    ("POISSON", "x, mean, cumulative", "Statistical"),
    ("POISSON.DIST", "x, mean, cumulative", "Statistical"),
    ("POWER", "number, power", "Math & Trig"),
    ("PPMT", "rate, per, nper, pv, [fv], [type]", "Financial"),
    ("PROB", "x_range, prob_range, lower_limit, [upper_limit]", "Statistical"),
    ("PRODUCT", "number1, [number2], ...", "Math & Trig"),
    ("PROPER", "text", "Text"),
    ("PV", "rate, nper, pmt, [fv], [type]", "Financial"),
    ("QUARTILE", "array, quart", "Statistical"),
    ("QUARTILE.EXC", "array, quart", "Statistical"),
    ("QUARTILE.INC", "array, quart", "Statistical"),
    ("QUOTIENT", "numerator, denominator", "Math & Trig"),
    ("RADIANS", "angle", "Math & Trig"),
    ("RAND", "", "Math & Trig"),
    ("RANDARRAY", "[rows], [columns], [min], [max], [integer]", "Math & Trig"),
    ("RANDBETWEEN", "bottom, top", "Math & Trig"),
    ("RANK", "number, ref, [order]", "Statistical"),
    ("RANK.AVG", "number, ref, [order]", "Statistical"),
    ("RANK.EQ", "number, ref, [order]", "Statistical"),
    ("RATE", "nper, pmt, pv, [fv], [type], [guess]", "Financial"),
    ("REDUCE", "[initial_value], array, lambda(accumulator, value)", "Logical"),
    ("REGEXEXTRACT", "text, pattern, [return_mode], [case_sensitivity]", "Text"),
    ("REGEXREPLACE", "text, pattern, replacement, [occurrence], [case_sensitivity]", "Text"),
    ("REGEXTEST", "text, pattern, [case_sensitivity]", "Text"),
    ("REPLACE", "old_text, start_num, num_chars, new_text", "Text"),
    ("REPLACEB", "old_text, start_num, num_bytes, new_text", "Text"),
    ("REPT", "text, number_times", "Text"),
    ("RIGHT", "text, [num_chars]", "Text"),
    ("RIGHTB", "text, [num_bytes]", "Text"),
    ("ROMAN", "number, [form]", "Math & Trig"),
    ("ROUND", "number, num_digits", "Math & Trig"),
    ("ROUNDDOWN", "number, num_digits", "Math & Trig"),
    ("ROUNDUP", "number, num_digits", "Math & Trig"),
    ("ROW", "[reference]", "Lookup & Reference"),
    ("ROWS", "array", "Lookup & Reference"),
    ("RRI", "nper, pv, fv", "Financial"),
    ("RSQ", "known_y's, known_x's", "Statistical"),
    ("SCAN", "[initial_value], array, lambda(accumulator, value)", "Logical"),
    ("SEARCH", "find_text, within_text, [start_num]", "Text"),
    ("SEARCHB", "find_text, within_text, [start_num]", "Text"),
    ("SEC", "number", "Math & Trig"),
    ("SECH", "number", "Math & Trig"),
    ("SECOND", "serial_number", "Date & Time"),
    ("SEQUENCE", "rows, [columns], [start], [step]", "Math & Trig"),
    ("SERIESSUM", "x, n, m, coefficients", "Math & Trig"),
    ("SHEET", "[value]", "Information"),
    ("SHEETS", "[reference]", "Information"),
    ("SIGN", "number", "Math & Trig"),
    ("SIN", "number", "Math & Trig"),
    ("SINH", "number", "Math & Trig"),
    ("SKEW", "number1, [number2], ...", "Statistical"),
    ("SKEW.P", "number1, [number2], ...", "Statistical"),
    ("SLN", "cost, salvage, life", "Financial"),
    ("SLOPE", "known_y's, known_x's", "Statistical"),
    ("SMALL", "array, k", "Statistical"),
    ("SORT", "array, [sort_index], [sort_order], [by_col]", "Lookup & Reference"),
    ("SORTBY", "array, by_array1, [sort_order1], ...", "Lookup & Reference"),
    ("SQRT", "number", "Math & Trig"),
    ("SQRTPI", "number", "Math & Trig"),
    ("STANDARDIZE", "x, mean, standard_dev", "Statistical"),
    ("STDEV", "number1, [number2], ...", "Statistical"),
    ("STDEV.P", "number1, [number2], ...", "Statistical"),
    ("STDEV.S", "number1, [number2], ...", "Statistical"),
    ("STDEVA", "value1, [value2], ...", "Statistical"),
    ("STDEVP", "number1, [number2], ...", "Statistical"),
    ("STDEVPA", "value1, [value2], ...", "Statistical"),
    ("STEYX", "known_y's, known_x's", "Statistical"),
    ("SUBSTITUTE", "text, old_text, new_text, [instance_num]", "Text"),
    ("SUBTOTAL", "function_num, ref1, [ref2], ...", "Math & Trig"),
    ("SUM", "number1, [number2], ...", "Math & Trig"),
    ("SUMIF", "range, criteria, [sum_range]", "Math & Trig"),
    ("SUMIFS", "sum_range, criteria_range1, criteria1, ...", "Math & Trig"),
    ("SUMPRODUCT", "array1, [array2], ...", "Math & Trig"),
    ("SUMSQ", "number1, [number2], ...", "Math & Trig"),
    ("SUMX2MY2", "array_x, array_y", "Math & Trig"),
    ("SUMX2PY2", "array_x, array_y", "Math & Trig"),
    ("SUMXMY2", "array_x, array_y", "Math & Trig"),
    ("SWITCH", "expression, value1, result1, ..., [default]", "Logical"),
    ("SYD", "cost, salvage, life, per", "Financial"),
    ("T", "value", "Text"),
    ("T.DIST", "x, deg_freedom, cumulative", "Statistical"),
    ("T.DIST.2T", "x, deg_freedom", "Statistical"),
    ("T.DIST.RT", "x, deg_freedom", "Statistical"),
    ("T.INV", "probability, deg_freedom", "Statistical"),
    ("T.INV.2T", "probability, deg_freedom", "Statistical"),
    ("T.TEST", "array1, array2, tails, type", "Statistical"),
    ("TAKE", "array, rows, [columns]", "Lookup & Reference"),
    ("TAN", "number", "Math & Trig"),
    ("TANH", "number", "Math & Trig"),
    ("TDIST", "x, deg_freedom, tails", "Statistical"),
    ("TEXT", "value, format_text", "Text"),
    ("TEXTAFTER", "text, delimiter, [instance_num]", "Text"),
    ("TEXTBEFORE", "text, delimiter, [instance_num]", "Text"),
    ("TEXTJOIN", "delimiter, ignore_empty, text1, ...", "Text"),
    ("TEXTSPLIT", "text, col_delimiter, [row_delimiter], [ignore_empty], [match_mode], [pad_with]", "Text"),
    ("TIME", "hour, minute, second", "Date & Time"),
    ("TIMEVALUE", "time_text", "Date & Time"),
    ("TINV", "probability, deg_freedom", "Statistical"),
    ("TOCOL", "array, [ignore], [scan_by_column]", "Lookup & Reference"),
    ("TODAY", "", "Date & Time"),
    ("TOROW", "array, [ignore], [scan_by_column]", "Lookup & Reference"),
    ("TRANSPOSE", "array", "Lookup & Reference"),
    ("TREND", "known_y's, [known_x's], [new_x's], [const]", "Statistical"),
    ("TRIM", "text", "Text"),
    ("TRIMMEAN", "array, percent", "Statistical"),
    ("TRUE", "", "Logical"),
    ("TRUNC", "number, [num_digits]", "Math & Trig"),
    ("TTEST", "array1, array2, tails, type", "Statistical"),
    ("TYPE", "value", "Information"),
    ("UNICHAR", "number", "Text"),
    ("UNICODE", "text", "Text"),
    ("UNIQUE", "array, [by_col], [exactly_once]", "Lookup & Reference"),
    ("UPPER", "text", "Text"),
    ("VALUE", "text", "Text"),
    ("VALUETOTEXT", "value, [format]", "Text"),
    ("VAR", "number1, [number2], ...", "Statistical"),
    ("VAR.P", "number1, [number2], ...", "Statistical"),
    ("VAR.S", "number1, [number2], ...", "Statistical"),
    ("VARA", "value1, [value2], ...", "Statistical"),
    ("VARP", "number1, [number2], ...", "Statistical"),
    ("VARPA", "value1, [value2], ...", "Statistical"),
    ("VLOOKUP", "lookup_value, table_array, col_index_num, [range_lookup]", "Lookup & Reference"),
    ("VSTACK", "array1, [array2], ...", "Lookup & Reference"),
    ("WEEKDAY", "serial_number, [return_type]", "Date & Time"),
    ("WEEKNUM", "serial_number, [return_type]", "Date & Time"),
    ("WEIBULL", "x, alpha, beta, cumulative", "Statistical"),
    ("WEIBULL.DIST", "x, alpha, beta, cumulative", "Statistical"),
    ("WORKDAY", "start_date, days, [holidays]", "Date & Time"),
    ("WORKDAY.INTL", "start_date, days, [weekend], [holidays]", "Date & Time"),
    ("WRAPCOLS", "vector, wrap_count, [pad_with]", "Lookup & Reference"),
    ("WRAPROWS", "vector, wrap_count, [pad_with]", "Lookup & Reference"),
    ("XIRR", "values, dates, [guess]", "Financial"),
    ("XLOOKUP", "lookup_value, lookup_array, return_array, [if_not_found], [match_mode], [search_mode]", "Lookup & Reference"),
    ("XMATCH", "lookup_value, lookup_array, [match_mode], [search_mode]", "Lookup & Reference"),
    ("XNPV", "rate, values, dates", "Financial"),
    ("XOR", "logical1, [logical2], ...", "Logical"),
    ("YEAR", "serial_number", "Date & Time"),
    ("YEARFRAC", "start_date, end_date, [basis]", "Date & Time"),
    ("Z.TEST", "array, x, [sigma]", "Statistical"),
    ("ZTEST", "array, x, [sigma]", "Statistical"),
];

pub fn function_names() -> impl Iterator<Item = (&'static str, &'static str)> {
    FUNCS.iter().map(|(n, _, c)| (*n, *c))
}

pub fn signature(name: &str) -> Option<&'static str> {
    FUNCS.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(name)).map(|(_, a, _)| *a)
}

/// The binary-searched name tables must stay sorted.
#[cfg(test)]
pub(super) fn check_lift_tables() {
    for t in [LIFT_ALL, LIFT_MORE, SCALAR_ONLY] {
        assert!(t.windows(2).all(|w| w[0] < w[1]), "unsorted near {:?}", t.windows(2).find(|w| w[0] >= w[1]));
    }
}

/// Whether `name` (upper case) is a built-in function rather than a named LAMBDA.
pub fn is_builtin(name: &str) -> bool {
    FUNCS.binary_search_by(|f| f.0.cmp(name)).is_ok() || name == "SINGLE"
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(super) fn need(a: &[Expr], min: usize, max: usize) -> Result<(), E> {
    if a.len() < min || a.len() > max { Err(E::Value) } else { Ok(()) }
}

pub(super) fn round_half_away(x: f64, digits: i32) -> f64 {
    let p = 10f64.powi(digits);
    let y = x * p;
    let frac = y.abs().fract();
    let r = if (frac - 0.5).abs() < 1e-9 { y.trunc() + y.signum() } else { y.round() };
    let out = r / p;
    if out == 0.0 { 0.0 } else { out }
}

fn wild_match(p: &[char], s: &[char]) -> bool {
    // Glob-style with '*', '?', and '~' escape. Case-insensitive.
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while si < s.len() {
        if pi < p.len() {
            let (pc, esc) = if p[pi] == '~' && pi + 1 < p.len() { (p[pi + 1], true) } else { (p[pi], false) };
            let step = if esc { 2 } else { 1 };
            if !esc && pc == '*' {
                star = Some(pi);
                mark = si;
                pi += 1;
                continue;
            }
            if (!esc && pc == '?') || pc.to_lowercase().eq(s[si].to_lowercase()) {
                pi += step;
                si += 1;
                continue;
            }
        }
        if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub(super) fn has_wild(s: &str) -> bool {
    s.contains(['*', '?', '~'])
}

/// `pattern*` for SEARCH: matches any text that starts with a match of `pattern`. A lone
/// trailing `~` (a literal tilde) is written as `~~` so it does not escape the added `*`.
fn search_prefix_pattern(p: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(p.len() + 2);
    let mut i = 0;
    while i < p.len() {
        if p[i] == '~' {
            out.push('~');
            out.push(p.get(i + 1).copied().unwrap_or('~'));
            i += 2;
        } else {
            out.push(p[i]);
            i += 1;
        }
    }
    out.push('*');
    out
}

/// Latest date serial (9999-12-31).
pub(super) const MAX_SERIAL: f64 = 2_958_465.0;

/// Checks a date serial argument: Excel's dates run from 0 to 9999-12-31, others are #NUM!.
pub(super) fn serial(x: f64) -> Result<f64, E> {
    if (0.0..MAX_SERIAL + 1.0).contains(&x) { Ok(x) } else { Err(E::Num) }
}

/// Holiday serials as a set (whole days), so per-day lookups stay cheap.
pub(super) fn holiday_set(days: impl IntoIterator<Item = f64>) -> std::collections::HashSet<i64> {
    days.into_iter().filter(|d| d.is_finite()).map(|d| d.floor() as i64).collect()
}

/// Longest WORKDAY offset accepted (in working days), and NETWORKDAYS span (in days).
pub(super) const WORKDAYS_MAX: f64 = 1_000_000.0;

pub(super) fn text_match(pattern: &str, s: &str) -> bool {
    if has_wild(pattern) {
        let p: Vec<char> = pattern.chars().collect();
        let sc: Vec<char> = s.chars().collect();
        wild_match(&p, &sc)
    } else {
        pattern.to_lowercase() == s.to_lowercase()
    }
}

#[derive(Debug)]
pub(super) enum CritOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

pub(super) struct Criteria {
    pub(super) op: CritOp,
    pub(super) val: Value,
}

pub(super) fn parse_criteria(v: &Value) -> Criteria {
    match v {
        Value::Text(s) => {
            let (op, rest) = if let Some(r) = s.strip_prefix(">=") {
                (CritOp::Ge, r)
            } else if let Some(r) = s.strip_prefix("<=") {
                (CritOp::Le, r)
            } else if let Some(r) = s.strip_prefix("<>") {
                (CritOp::Ne, r)
            } else if let Some(r) = s.strip_prefix('>') {
                (CritOp::Gt, r)
            } else if let Some(r) = s.strip_prefix('<') {
                (CritOp::Lt, r)
            } else if let Some(r) = s.strip_prefix('=') {
                (CritOp::Eq, r)
            } else {
                (CritOp::Eq, s.as_str())
            };
            let val = if rest.is_empty() {
                Value::Empty
            } else {
                // Numbers, dates ("1/15/2024", "2024-01-15"), date-times, times, percents, logicals.
                match numfmt::parse_input(rest).0 {
                    v @ (Value::Number(_) | Value::Bool(_)) => v,
                    _ => Value::Text(rest.to_string()),
                }
            };
            Criteria { op, val }
        }
        v => Criteria { op: CritOp::Eq, val: v.clone() },
    }
}

pub(super) fn crit_match(c: &Criteria, v: &Value) -> bool {
    match (&c.op, &c.val) {
        (CritOp::Eq, Value::Empty) => matches!(v, Value::Empty) || matches!(v, Value::Text(s) if s.is_empty()),
        (CritOp::Ne, Value::Empty) => !(matches!(v, Value::Empty) || matches!(v, Value::Text(s) if s.is_empty())),
        (CritOp::Eq, Value::Text(p)) => match v {
            Value::Text(s) => text_match(p, s),
            _ => false,
        },
        (CritOp::Ne, Value::Text(p)) => match v {
            Value::Text(s) => !text_match(p, s),
            _ => true,
        },
        (CritOp::Eq, Value::Number(n)) => match v {
            Value::Number(x) => x == n,
            Value::Text(s) => numfmt::parse_input(s).0 == Value::Number(*n),
            _ => false,
        },
        (CritOp::Ne, Value::Number(n)) => !matches!(v, Value::Number(x) if x == n),
        (CritOp::Eq, val) => values_equal(v, val) && std::mem::discriminant(v) == std::mem::discriminant(val),
        (CritOp::Ne, val) => !values_equal(v, val),
        (op, val) => {
            // ordering comparisons only between same kinds
            let same = matches!((v, val), (Value::Number(_), Value::Number(_)) | (Value::Text(_), Value::Text(_)) | (Value::Bool(_), Value::Bool(_)));
            if !same {
                return false;
            }
            let o = compare(v, val);
            match op {
                CritOp::Lt => o == Ordering::Less,
                CritOp::Le => o != Ordering::Greater,
                CritOp::Gt => o == Ordering::Greater,
                CritOp::Ge => o != Ordering::Less,
                _ => false,
            }
        }
    }
}

pub(super) fn get(g: &[Vec<Value>], r: usize, c: usize) -> Value {
    g.get(r).and_then(|row| row.get(c)).cloned().unwrap_or(Value::Empty)
}

/// Grids of the *IF(S) range arguments over one logical shape: that of the first argument
/// (the others are re-anchored at their top-left cell with the same size, like Excel). Only
/// the leading block that holds data in any of them is materialised, so whole-column and
/// cross-sheet ranges stay cheap without losing rows. Returns the grids, the block's
/// (rows, cols) and the number of logical cells outside it (blank in every argument).
pub(super) fn cond_grids(ev: &mut Evaluator, args: Vec<EV>) -> (Vec<Vec<Vec<Value>>>, usize, usize, f64) {
    let dims = |v: &EV| match v {
        EV::R(_, rg) => (rg.rows() as usize, rg.cols() as usize),
        EV::A(g) => (g.len(), g.first().map_or(0, |r| r.len())),
        _ => (1, 1),
    };
    let (rows, cols) = args.first().map_or((0, 0), dims);
    let (mut used_r, mut used_c) = (0, 0);
    for v in &args {
        let (r, c) = match v {
            EV::R(s, rg) => match ev.extent(*s) {
                Some((mr, mc)) if mr >= rg.r1 && mc >= rg.c1 => ((mr - rg.r1 + 1) as usize, (mc - rg.c1 + 1) as usize),
                _ => (0, 0),
            },
            other => dims(other),
        };
        used_r = used_r.max(r.min(rows));
        used_c = used_c.max(c.min(cols));
    }
    let grids = args
        .into_iter()
        .map(|v| match v {
            EV::R(s, rg) if used_r > 0 && used_c > 0 => {
                let r2 = (rg.r1 as usize + used_r - 1).min(MAX_ROWS as usize - 1) as u32;
                let c2 = (rg.c1 as usize + used_c - 1).min(MAX_COLS as usize - 1) as u32;
                ev.grid(EV::R(s, Range::new(rg.r1, rg.c1, r2, c2)))
            }
            EV::R(..) => Vec::new(),
            other => ev.grid(other),
        })
        .collect();
    let outside = rows as f64 * cols as f64 - used_r as f64 * used_c as f64;
    (grids, used_r, used_c, outside)
}

/// Grids of arguments combined cell by cell where blank cells cannot change the result
/// (SUMPRODUCT, paired statistics): ranges of one size are clipped together to their common
/// data block instead of being materialised whole; anything else is materialised as is.
pub(super) fn aligned_grids(ev: &mut Evaluator, vals: Vec<EV>) -> Vec<Vec<Vec<Value>>> {
    let size = |v: &EV| match v {
        EV::R(_, rg) => Some((rg.rows(), rg.cols())),
        _ => None,
    };
    let first = vals.first().and_then(size);
    if first.is_some() && vals.iter().all(|v| size(v) == first) {
        cond_grids(ev, vals).0
    } else {
        vals.into_iter().map(|v| ev.grid(v)).collect()
    }
}

/// (range, criteria) pairs of COUNTIFS/SUMIFS-style arguments from `start` on.
fn criteria_pairs(ev: &mut Evaluator, a: &[Expr], start: usize) -> Result<(Vec<EV>, Vec<Criteria>), E> {
    if start >= a.len() || (a.len() - start) % 2 != 0 {
        return Err(E::Value);
    }
    let (mut ranges, mut crits) = (Vec::new(), Vec::new());
    for i in (start..a.len()).step_by(2) {
        ranges.push(ev.eval(&a[i])?);
        crits.push(parse_criteria(&ev.value(a, i + 1)?));
    }
    Ok((ranges, crits))
}

/// Flattens a grid into a vector (row-major) and reports whether it was a column.
pub(super) fn vector(g: Vec<Vec<Value>>) -> Vec<Value> {
    if g.len() == 1 {
        g.into_iter().next().unwrap()
    } else {
        g.into_iter().flat_map(|r| r.into_iter().take(1)).collect()
    }
}

pub(super) fn lookup_exact(vals: &[Value], key: &Value, wildcard: bool, reverse: bool) -> Option<usize> {
    let idx: Box<dyn Iterator<Item = usize>> = if reverse { Box::new((0..vals.len()).rev()) } else { Box::new(0..vals.len()) };
    for i in idx {
        let v = &vals[i];
        let hit = match (key, v) {
            (Value::Text(k), Value::Text(s)) if wildcard => text_match(k, s),
            (Value::Text(k), Value::Text(s)) => k.to_lowercase() == s.to_lowercase(),
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            _ => false,
        };
        if hit {
            return Some(i);
        }
    }
    None
}

/// Largest value <= key (sorted ascending assumption, linear scan like a binary search would see).
pub(super) fn lookup_approx(vals: &[Value], key: &Value) -> Option<usize> {
    let mut best = None;
    for (i, v) in vals.iter().enumerate() {
        if v.is_empty() || std::mem::discriminant(v) != std::mem::discriminant(key) {
            continue;
        }
        if compare(v, key) != Ordering::Greater {
            best = Some(i);
        } else {
            break;
        }
    }
    best
}

pub(super) fn lookup_mode(vals: &[Value], key: &Value, mode: i64, reverse: bool) -> Option<usize> {
    if let Some(i) = lookup_exact(vals, key, mode == 2, reverse) {
        return Some(i);
    }
    if mode == -1 || mode == 1 {
        let mut best: Option<usize> = None;
        for (i, v) in vals.iter().enumerate() {
            if std::mem::discriminant(v) != std::mem::discriminant(key) {
                continue;
            }
            let o = compare(v, key);
            let ok = if mode == -1 { o == Ordering::Less } else { o == Ordering::Greater };
            if ok {
                best = match best {
                    None => Some(i),
                    Some(b) => {
                        let ob = compare(v, &vals[b]);
                        if (mode == -1 && ob == Ordering::Greater) || (mode == 1 && ob == Ordering::Less) { Some(i) } else { Some(b) }
                    }
                };
            }
        }
        return best;
    }
    None
}

pub(super) fn stats(v: &[f64], sample: bool) -> Result<f64, E> {
    let n = v.len() as f64;
    if n < if sample { 2.0 } else { 1.0 } {
        return Err(E::Div0);
    }
    let mean = v.iter().sum::<f64>() / n;
    let ss: f64 = v.iter().map(|x| (x - mean).powi(2)).sum();
    Ok(ss / if sample { n - 1.0 } else { n })
}

pub(super) fn percentile(mut v: Vec<f64>, k: f64) -> Result<f64, E> {
    if v.is_empty() || !(0.0..=1.0).contains(&k) {
        return Err(E::Num);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let pos = k * (v.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    Ok(v[lo] + (v[hi] - v[lo]) * (pos - lo as f64))
}

pub(super) fn gcd(a: f64, b: f64) -> f64 {
    let (mut a, mut b) = (a.abs().floor(), b.abs().floor());
    while b > 0.0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

pub(super) fn add_months(serial: f64, months: i64, eom: bool) -> f64 {
    let (y, m, d) = numfmt::serial_to_ymd(serial);
    let total = y * 12 + (m - 1) + months;
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) + 1);
    let dim = numfmt::days_in_month(ny, nm);
    let nd = if eom { dim } else { d.min(dim) };
    numfmt::ymd_to_serial(ny, nm, nd)
}

pub(super) fn pmt(rate: f64, nper: f64, pv: f64, fv: f64, typ: f64) -> f64 {
    if rate == 0.0 {
        return -(pv + fv) / nper;
    }
    let p = (1.0 + rate).powf(nper);
    -(rate * (pv * p + fv)) / ((1.0 + rate * typ) * (p - 1.0))
}

pub(super) fn fv(rate: f64, nper: f64, pmt: f64, pv: f64, typ: f64) -> f64 {
    if rate == 0.0 {
        return -(pv + pmt * nper);
    }
    let p = (1.0 + rate).powf(nper);
    -(pv * p + pmt * (1.0 + rate * typ) * (p - 1.0) / rate)
}

pub(super) fn ipmt(rate: f64, per: f64, nper: f64, pv: f64, fvv: f64, typ: f64) -> f64 {
    let p = pmt(rate, nper, pv, fvv, typ);
    let i = fv(rate, per - 1.0, p, pv, typ) * rate;
    if typ == 1.0 {
        if per == 1.0 { 0.0 } else { i / (1.0 + rate) }
    } else {
        i
    }
}

pub(super) fn is_weekend(serial: f64) -> bool {
    let w = numfmt::weekday(serial);
    w == 1 || w == 7
}

pub(super) fn sort_grid(mut g: Vec<Vec<Value>>, col: usize, desc: bool) -> Vec<Vec<Value>> {
    g.sort_by(|a, b| {
        let o = compare(&a.get(col).cloned().unwrap_or_default(), &b.get(col).cloned().unwrap_or_default());
        if desc { o.reverse() } else { o }
    });
    g
}

pub(super) fn transpose(g: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    let cols = g.first().map_or(0, |r| r.len());
    (0..cols).map(|c| g.iter().map(|r| r.get(c).cloned().unwrap_or_default()).collect()).collect()
}

fn is_ref(e: &Expr) -> bool {
    matches!(e, Expr::Ref(_))
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Scalar functions whose arguments are evaluated element-wise over arrays ("lifting").
#[derive(Clone, Copy)]
enum Lift {
    All,
    First,
    At(usize),
    /// Criteria arguments: start index, then every second one.
    Pairs(usize),
}

impl Lift {
    fn lifts(self, i: usize) -> bool {
        match self {
            Lift::All => true,
            Lift::First => i == 0,
            Lift::At(k) => i == k,
            Lift::Pairs(k) => i >= k && (i - k) % 2 == 0,
        }
    }
}

/// Functions taking only scalar arguments (sorted).
const LIFT_ALL: &[&str] = &[
    "ABS", "ACOS", "ADDRESS", "ARABIC", "ASIN", "ATAN", "ATAN2", "BASE", "BIN2DEC", "CEILING", "CEILING.MATH", "CHAR", "CLEAN",
    "CODE", "COMBIN", "COMBINA", "CONVERT", "COS", "DATE", "DATEDIF", "DATEVALUE", "DAY", "DAYS", "DAYS360", "DEC2BIN", "DEC2HEX",
    "DEC2OCT", "DECIMAL", "DEGREES", "DOLLAR", "EDATE", "EFFECT", "EOMONTH", "EVEN", "EXACT", "EXP", "FACT", "FIND", "FIXED",
    "FLOOR", "FLOOR.MATH", "HEX2DEC", "HOUR", "HYPERLINK", "IF", "IFERROR", "IFNA", "INT", "ISBLANK", "ISERR", "ISERROR",
    "ISEVEN", "ISLOGICAL", "ISNA", "ISNONTEXT", "ISNUMBER", "ISODD", "ISOWEEKNUM", "ISTEXT", "LEFT", "LEN", "LN", "LOG",
    "LOG10", "LOWER", "MID", "MINUTE", "MOD", "MONTH", "MROUND", "N", "NOMINAL", "NORM.DIST", "NORM.INV", "NORM.S.DIST",
    "NORM.S.INV", "NOT", "NUMBERVALUE", "OCT2DEC", "ODD", "PERMUT", "POWER", "PROPER", "QUOTIENT", "RADIANS", "REPLACE",
    "REPT", "RIGHT", "ROMAN", "ROUND", "ROUNDDOWN", "ROUNDUP", "SEARCH", "SECOND", "SIGN", "SIN", "SQRT", "STANDARDIZE",
    "SUBSTITUTE", "T", "T.DIST", "T.INV", "TAN", "TEXT", "TEXTAFTER", "TEXTBEFORE", "TIME", "TIMEVALUE", "TRIM", "TRUNC",
    "UNICHAR", "UNICODE", "UPPER", "VALUE", "WEEKDAY", "WEEKNUM", "YEAR", "YEARFRAC",
];

/// More scalar-argument functions (distributions, hyperbolic, engineering; sorted).
const LIFT_MORE: &[&str] = &[
    "ACOSH", "ACOT", "ACOTH", "ASINH", "ATANH", "BETA.DIST", "BETA.INV", "BETADIST", "BETAINV", "BINOM.DIST", "BINOM.INV",
    "BINOMDIST", "BITAND", "BITLSHIFT", "BITOR", "BITRSHIFT", "BITXOR", "CEILING.PRECISE", "CHIDIST", "CHIINV", "CHISQ.DIST",
    "CHISQ.DIST.RT", "CHISQ.INV", "CHISQ.INV.RT", "COSH", "COT", "COTH", "CRITBINOM", "CSC", "CSCH", "DELTA", "DOLLARDE",
    "DOLLARFR", "ENCODEURL", "ERF", "ERF.PRECISE", "ERFC", "ERFC.PRECISE", "EXPON.DIST", "EXPONDIST", "F.DIST", "F.DIST.RT",
    "F.INV", "F.INV.RT", "FACTDOUBLE", "FDIST", "FINV", "FISHER", "FISHERINV", "FLOOR.PRECISE", "GAMMA", "GAMMA.DIST",
    "GAMMA.INV", "GAMMADIST", "GAMMAINV", "GAMMALN", "GAMMALN.PRECISE", "GAUSS", "GESTEP", "HYPGEOM.DIST", "HYPGEOMDIST",
    "ISO.CEILING", "ISPMT", "LOGINV", "LOGNORM.DIST", "LOGNORM.INV", "LOGNORMDIST", "NEGBINOM.DIST", "NEGBINOMDIST",
    "PERMUTATIONA", "PHI", "POISSON", "POISSON.DIST", "REGEXREPLACE", "REGEXTEST", "SEC", "SECH", "SINH", "SQRTPI", "TANH",
    "VALUETOTEXT", "WEIBULL", "WEIBULL.DIST",
];

/// Functions that always return a single value regardless of their arguments (sorted).
const SCALAR_ONLY: &[&str] = &[
    "AGGREGATE", "AND", "AVERAGE", "AVERAGEA", "COLUMNS", "CONCAT", "CONCATENATE", "CORREL", "COUNT", "COUNTA", "COUNTBLANK",
    "FORMULATEXT", "GETPIVOTDATA", "ISFORMULA", "ISREF", "LARGE", "MAX", "MAXA", "MEDIAN", "MIN", "MINA", "NOW", "NPV", "OR", "PI", "PRODUCT",
    "RAND", "ROWS", "SHEET", "SHEETS", "SMALL", "STDEV", "STDEV.P", "STDEV.S", "SUBTOTAL", "SUM", "SUMPRODUCT", "SUMSQ",
    "TEXTJOIN", "TODAY", "VAR", "VAR.P", "VAR.S", "XOR",
];

fn lift_spec(name: &str) -> Option<Lift> {
    match name {
        "VLOOKUP" | "HLOOKUP" | "MATCH" | "XMATCH" => Some(Lift::First),
        "COUNTIF" | "SUMIF" | "AVERAGEIF" => Some(Lift::At(1)),
        "COUNTIFS" => Some(Lift::Pairs(1)),
        "SUMIFS" | "AVERAGEIFS" | "MAXIFS" | "MINIFS" => Some(Lift::Pairs(2)),
        _ if LIFT_ALL.binary_search(&name).is_ok() || LIFT_MORE.binary_search(&name).is_ok() => Some(Lift::All),
        _ => None,
    }
}

/// Conservative static check: could this expression evaluate to more than one cell?
fn maybe_multi(e: &Expr) -> bool {
    match e {
        Expr::Ref(r) => r.kind != RefKind::Cell,
        Expr::Array(rows) => rows.len() > 1 || rows.first().is_some_and(|r| r.len() > 1),
        Expr::Name(_) | Expr::Range(..) | Expr::Isect(..) | Expr::Union(_) | Expr::Call(..) => true,
        Expr::Neg(a) | Expr::Plus(a) | Expr::Percent(a) | Expr::Paren(a) => maybe_multi(a),
        Expr::Bin(_, a, b) => maybe_multi(a) || maybe_multi(b),
        Expr::Func(n, args) => {
            if n == "SINGLE" || SCALAR_ONLY.binary_search(&n.as_str()).is_ok() {
                false
            } else if let Some(spec) = lift_spec(n) {
                args.iter().enumerate().any(|(i, a)| spec.lifts(i) && maybe_multi(a))
            } else {
                true
            }
        }
        _ => false,
    }
}

/// Calls a worksheet function, evaluating scalar arguments element-wise when given arrays.
pub fn call(ev: &mut Evaluator, name: &str, a: &[Expr]) -> Res {
    if let Some(spec) = lift_spec(name) {
        let trigger = if matches!(name, "IF" | "IFERROR" | "IFNA") {
            a.first().is_some_and(maybe_multi)
        } else {
            a.iter().enumerate().any(|(i, e)| spec.lifts(i) && maybe_multi(e))
        };
        if trigger {
            return lift(ev, name, a, spec);
        }
    }
    dispatch(ev, name, a)
}

fn lift(ev: &mut Evaluator, name: &str, a: &[Expr], spec: Lift) -> Res {
    let idx: Vec<usize> = (0..a.len()).filter(|&i| spec.lifts(i) && maybe_multi(&a[i])).collect();
    let mut vals = Vec::with_capacity(idx.len());
    let lifted = |ev: &mut Evaluator, i: usize| match ev.eval(&a[i]) {
        Ok(EV::R(s, rg)) => EV::R(s, ev.clip(s, rg)),
        Ok(v) => v,
        Err(e) => EV::V(Value::Error(e)),
    };
    // IF / IFERROR / IFNA with a single-valued first argument evaluate only the branch taken
    // (recursive LAMBDAs rely on it).
    if matches!(name, "IF" | "IFERROR" | "IFNA") && idx.first() == Some(&0) {
        let first = lifted(ev, 0);
        if !ev.is_multi(&first) {
            let mut args = a.to_vec();
            args[0] = Expr::Name("\u{1}0".into());
            ev.bind("\u{1}0".into(), first);
            let r = dispatch(ev, name, &args);
            ev.unbind(1);
            return r;
        }
        vals.push(first);
    }
    for &i in &idx[vals.len()..] {
        let v = lifted(ev, i);
        vals.push(v);
    }
    let names: Vec<String> = idx.iter().map(|i| format!("\u{1}{i}")).collect();
    let mut args = a.to_vec();
    for (k, &i) in idx.iter().enumerate() {
        args[i] = Expr::Name(names[k].clone());
    }
    if !vals.iter().any(|v| ev.is_multi(v)) {
        for (k, v) in vals.into_iter().enumerate() {
            ev.bind(names[k].clone(), v);
        }
        let r = dispatch(ev, name, &args);
        ev.unbind(idx.len());
        return r;
    }
    let dims = |v: &EV| match v {
        EV::R(_, rg) => (rg.rows() as usize, rg.cols() as usize),
        EV::A(g) => (g.len(), g.first().map_or(0, |r| r.len())),
        _ => (1, 1),
    };
    let rows = vals.iter().map(|v| dims(v).0).max().unwrap_or(1);
    let cols = vals.iter().map(|v| dims(v).1).max().unwrap_or(1);
    if rows as u64 * cols as u64 > 1_048_576 {
        return Err(E::Calc);
    }
    let mut out = vec![vec![Value::Empty; cols]; rows];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, slot) in row.iter_mut().enumerate() {
            for (k, v) in vals.iter().enumerate() {
                ev.bind(names[k].clone(), element(v, r, c));
            }
            let res = dispatch(ev, name, &args);
            ev.unbind(idx.len());
            *slot = match res {
                Err(e) => Value::Error(e),
                Ok(EV::V(v)) => v,
                Ok(other) => ev.scalar(other).unwrap_or_else(Value::Error),
            };
        }
    }
    Ok(EV::A(out))
}

/// Element (r, c) of an argument under array broadcasting.
pub(super) fn element(v: &EV, r: usize, c: usize) -> EV {
    let pick = |h: usize, w: usize| -> Option<(usize, usize)> {
        let rr = if h == 1 { 0 } else { r };
        let cc = if w == 1 { 0 } else { c };
        (rr < h && cc < w).then_some((rr, cc))
    };
    match v {
        EV::R(s, rg) => match pick(rg.rows() as usize, rg.cols() as usize) {
            Some((rr, cc)) => EV::R(*s, Range::cell(rg.r1 + rr as u32, rg.c1 + cc as u32)),
            None => EV::V(Value::Error(E::NA)),
        },
        EV::A(g) => match pick(g.len(), g.first().map_or(0, |x| x.len())).and_then(|(rr, cc)| g.get(rr)?.get(cc)) {
            Some(v) => EV::V(v.clone()),
            None => EV::V(Value::Error(E::NA)),
        },
        EV::V(_) | EV::L(_) => v.clone(),
        EV::M(_) => EV::V(Value::Error(E::Value)),
    }
}

/// Lookup vector for a range or array argument.
fn lookup_vector(ev: &mut Evaluator, v: EV) -> Rc<LookupCol> {
    match v {
        EV::R(s, rg) if rg.rows() == 1 || rg.cols() == 1 => ev.lookup_col(s, rg),
        other => Rc::new(LookupCol::new(vector(ev.grid(other)))),
    }
}

/// Exact match position (optionally with wildcards), first or last occurrence.
fn find_exact(col: &LookupCol, key: &Value, wildcard: bool, reverse: bool) -> Option<usize> {
    if (wildcard && matches!(key, Value::Text(k) if has_wild(k))) || col.vals.len() < 8 {
        lookup_exact(&col.vals, key, wildcard, reverse)
    } else {
        col.find(key, reverse)
    }
}

fn dispatch(ev: &mut Evaluator, name: &str, a: &[Expr]) -> Res {
    let n = a.len();
    match name {
        // What-If data table cell (written by Data > What-If Analysis > Data Table).
        "TABLE" => ev.data_table(),
        // ----- Math -----
        "SUM" => num(ev.numbers(a, false)?.iter().sum()),
        "PRODUCT" => num(ev.numbers(a, false)?.iter().product()),
        "SUMSQ" => num(ev.numbers(a, false)?.iter().map(|x| x * x).sum()),
        "ABS" => num(ev.number(a, 0)?.abs()),
        "SIGN" => {
            let x = ev.number(a, 0)?;
            num(if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 })
        }
        "INT" => num(ev.number(a, 0)?.floor()),
        "TRUNC" => {
            let x = ev.number(a, 0)?;
            let d = ev.number_or(a, 1, 0.0)? as i32;
            let p = 10f64.powi(d);
            num((x * p).trunc() / p)
        }
        "ROUND" => {
            need(a, 2, 2)?;
            num(round_half_away(ev.number(a, 0)?, ev.number(a, 1)? as i32))
        }
        "ROUNDUP" | "ROUNDDOWN" => {
            need(a, 2, 2)?;
            let x = ev.number(a, 0)?;
            let p = 10f64.powi(ev.number(a, 1)? as i32);
            let y = x * p;
            let y = if (y - y.round()).abs() < 1e-9 { y.round() } else { y };
            num(if name == "ROUNDUP" { y.abs().ceil() * y.signum() } else { y.trunc() } / p)
        }
        "MROUND" => {
            let x = ev.number(a, 0)?;
            let m = ev.number(a, 1)?;
            if m == 0.0 {
                return num(0.0);
            }
            if x * m < 0.0 {
                return Err(E::Num);
            }
            num(round_half_away(x / m, 0) * m)
        }
        "CEILING" | "CEILING.MATH" | "FLOOR" | "FLOOR.MATH" => {
            let x = ev.number(a, 0)?;
            let s = ev.number_or(a, 1, 1.0)?;
            if s == 0.0 {
                return num(0.0);
            }
            let up = name.starts_with("CEILING");
            let mode = ev.number_or(a, 2, 0.0)?;
            let q = x / s.abs();
            let r = if name.ends_with(".MATH") && x < 0.0 && mode != 0.0 {
                if up { q.floor() } else { q.ceil() }
            } else if up {
                q.ceil()
            } else {
                q.floor()
            };
            num(r * s.abs())
        }
        "MOD" => {
            let x = ev.number(a, 0)?;
            let d = ev.number(a, 1)?;
            if d == 0.0 {
                return Err(E::Div0);
            }
            num(x - d * (x / d).floor())
        }
        "QUOTIENT" => {
            let x = ev.number(a, 0)?;
            let d = ev.number(a, 1)?;
            if d == 0.0 {
                return Err(E::Div0);
            }
            num((x / d).trunc())
        }
        "POWER" => {
            let x = ev.number(a, 0)?;
            let y = ev.number(a, 1)?;
            num(x.powf(y))
        }
        "SQRT" => {
            let x = ev.number(a, 0)?;
            if x < 0.0 { Err(E::Num) } else { num(x.sqrt()) }
        }
        "EXP" => num(ev.number(a, 0)?.exp()),
        "LN" => {
            let x = ev.number(a, 0)?;
            if x <= 0.0 { Err(E::Num) } else { num(x.ln()) }
        }
        "LOG10" => {
            let x = ev.number(a, 0)?;
            if x <= 0.0 { Err(E::Num) } else { num(x.log10()) }
        }
        "LOG" => {
            let x = ev.number(a, 0)?;
            let b = ev.number_or(a, 1, 10.0)?;
            if x <= 0.0 || b <= 0.0 || b == 1.0 { Err(E::Num) } else { num(x.ln() / b.ln()) }
        }
        "PI" => num(std::f64::consts::PI),
        "RAND" => num(ev.random()),
        "RANDBETWEEN" => {
            let lo = ev.number(a, 0)?.ceil();
            let hi = ev.number(a, 1)?.floor();
            if hi < lo {
                return Err(E::Num);
            }
            let r = ev.random();
            num(lo + (r * (hi - lo + 1.0)).floor())
        }
        "SIN" => num(ev.number(a, 0)?.sin()),
        "COS" => num(ev.number(a, 0)?.cos()),
        "TAN" => num(ev.number(a, 0)?.tan()),
        "ASIN" => num(ev.number(a, 0)?.asin()),
        "ACOS" => num(ev.number(a, 0)?.acos()),
        "ATAN" => num(ev.number(a, 0)?.atan()),
        "ATAN2" => {
            let x = ev.number(a, 0)?;
            let y = ev.number(a, 1)?;
            if x == 0.0 && y == 0.0 { Err(E::Div0) } else { num(y.atan2(x)) }
        }
        "DEGREES" => num(ev.number(a, 0)?.to_degrees()),
        "RADIANS" => num(ev.number(a, 0)?.to_radians()),
        "FACT" => {
            let x = ev.number(a, 0)?.floor();
            // 171! overflows a double; larger arguments would also loop for ages.
            if !(0.0..=170.0).contains(&x) {
                return Err(E::Num);
            }
            num((1..=x as u64).fold(1.0, |acc, k| acc * k as f64))
        }
        "EVEN" | "ODD" => {
            let x = ev.number(a, 0)?;
            let s = if x < 0.0 { -1.0 } else { 1.0 };
            let mut v = x.abs().ceil();
            let want_even = name == "EVEN";
            if (v % 2.0 == 0.0) != want_even {
                v += 1.0;
            }
            if !want_even && v == 0.0 {
                v = 1.0;
            }
            num(s * v)
        }
        "GCD" => num(ev.numbers(a, false)?.into_iter().fold(0.0, gcd)),
        "LCM" => num(ev.numbers(a, false)?.into_iter().fold(1.0, |acc, x| {
            let x = x.floor();
            if x == 0.0 || acc == 0.0 { 0.0 } else { acc * x / gcd(acc, x) }
        })),
        "SUMPRODUCT" => {
            let mut vals = Vec::with_capacity(n);
            for i in 0..n {
                vals.push(ev.arg(a, i)?);
            }
            let mut acc: Option<Vec<Vec<Value>>> = None;
            for g in aligned_grids(ev, vals) {
                acc = Some(match acc {
                    None => g,
                    Some(prev) => {
                        if prev.len() != g.len() || prev.first().map(|r| r.len()) != g.first().map(|r| r.len()) {
                            return Err(E::Value);
                        }
                        super::eval::broadcast(&prev, &g, |x, y| {
                            let fx = if let Value::Number(n) = x { *n } else if let Value::Error(e) = x { return Value::Error(*e) } else { 0.0 };
                            let fy = if let Value::Number(n) = y { *n } else if let Value::Error(e) = y { return Value::Error(*e) } else { 0.0 };
                            Value::Number(fx * fy)
                        })
                    }
                });
            }
            let mut sum = 0.0;
            for row in acc.unwrap_or_default() {
                for v in row {
                    match v {
                        Value::Number(x) => sum += x,
                        Value::Bool(true) if n == 1 => sum += 1.0,
                        Value::Error(e) => return Err(e),
                        _ => {}
                    }
                }
            }
            num(sum)
        }
        "SUBTOTAL" => {
            let f = ev.number(a, 0)? as i64;
            let skip = f > 100;
            let f = f % 100;
            // Cells that are themselves subtotals are ignored (no double counting).
            let nums = ev.numbers_opts(&a[1.min(n)..], skip, true)?;
            subtotal(f, nums)
        }
        "SEQUENCE" => {
            // Validate as floats: huge counts must not wrap when cast or multiplied.
            let rows = ev.number(a, 0)?.trunc();
            let cols = ev.number_or(a, 1, 1.0)?.trunc();
            let start = ev.number_or(a, 2, 1.0)?;
            let step = ev.number_or(a, 3, 1.0)?;
            if rows < 1.0 || cols < 1.0 || rows * cols > 1_000_000.0 {
                return Err(E::Calc);
            }
            let (rows, cols) = (rows as usize, cols as usize);
            Ok(EV::A((0..rows).map(|r| (0..cols).map(|c| Value::Number(start + step * (r * cols + c) as f64)).collect()).collect()))
        }

        // ----- Conditional aggregates -----
        "SUMIF" | "AVERAGEIF" => {
            need(a, 2, 3)?;
            let mut args = vec![ev.arg(a, 0)?];
            let crit = parse_criteria(&ev.value(a, 1)?);
            if ev.has(a, 2) {
                args.push(ev.arg(a, 2)?);
            }
            // Cells outside the data block are blank in the sum range: they add nothing.
            let (gs, rows, cols, _) = cond_grids(ev, args);
            let g = &gs[0];
            let sg = gs.get(1).unwrap_or(g);
            let (mut sum, mut cnt) = (0.0, 0.0);
            for r in 0..rows {
                for c in 0..cols {
                    if crit_match(&crit, &get(g, r, c)) {
                        match get(sg, r, c) {
                            Value::Number(x) => {
                                sum += x;
                                cnt += 1.0;
                            }
                            Value::Error(e) => return Err(e),
                            _ => {}
                        }
                    }
                }
            }
            if name == "SUMIF" { num(sum) } else if cnt == 0.0 { Err(E::Div0) } else { num(sum / cnt) }
        }
        "SUMIFS" | "AVERAGEIFS" | "MAXIFS" | "MINIFS" => {
            if n < 3 {
                return Err(E::Value);
            }
            let sv = ev.arg(a, 0)?;
            let (ranges, crits) = criteria_pairs(ev, a, 1)?;
            let mut args = vec![sv];
            args.extend(ranges);
            // Cells outside the data block are blank in the sum range: they add nothing.
            let (gs, rows, cols, _) = cond_grids(ev, args);
            let mut vals = Vec::new();
            for r in 0..rows {
                for c in 0..cols {
                    if crits.iter().zip(&gs[1..]).all(|(cr, g)| crit_match(cr, &get(g, r, c))) {
                        match get(&gs[0], r, c) {
                            Value::Number(x) => vals.push(x),
                            Value::Error(e) => return Err(e),
                            _ => {}
                        }
                    }
                }
            }
            match name {
                "SUMIFS" => num(vals.iter().sum()),
                "AVERAGEIFS" => {
                    if vals.is_empty() { Err(E::Div0) } else { num(vals.iter().sum::<f64>() / vals.len() as f64) }
                }
                "MAXIFS" => num(vals.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(if vals.is_empty() { 0.0 } else { f64::NEG_INFINITY })),
                _ => num(if vals.is_empty() { 0.0 } else { vals.iter().copied().fold(f64::INFINITY, f64::min) }),
            }
        }
        "COUNTIF" => {
            need(a, 2, 2)?;
            let rv = ev.arg(a, 0)?;
            let area = match &rv {
                EV::R(_, rg) => rg.rows() as f64 * rg.cols() as f64,
                _ => 0.0,
            };
            let g = ev.grid_clipped(rv);
            let crit = parse_criteria(&ev.value(a, 1)?);
            let seen = g.iter().map(|r| r.len()).sum::<usize>() as f64;
            // Cells beyond the used area are blank.
            let blanks = if crit_match(&crit, &Value::Empty) { (area - seen).max(0.0) } else { 0.0 };
            num(g.iter().flatten().filter(|v| crit_match(&crit, v)).count() as f64 + blanks)
        }
        "COUNTIFS" => {
            if n < 2 {
                return Err(E::Value);
            }
            let (ranges, crits) = criteria_pairs(ev, a, 0)?;
            let (gs, rows, cols, outside) = cond_grids(ev, ranges);
            let hits = |r: usize, c: usize| crits.iter().zip(&gs).all(|(cr, g)| crit_match(cr, &get(g, r, c)));
            let mut count = (0..rows).map(|r| (0..cols).filter(|&c| hits(r, c)).count()).sum::<usize>() as f64;
            // Cells beyond every range's data are blank: count them when blanks match.
            if crits.iter().all(|cr| crit_match(cr, &Value::Empty)) {
                count += outside;
            }
            num(count)
        }

        // ----- Statistics -----
        "AVERAGE" => {
            let v = ev.numbers(a, false)?;
            if v.is_empty() { Err(E::Div0) } else { num(v.iter().sum::<f64>() / v.len() as f64) }
        }
        "AVERAGEA" | "MAXA" | "MINA" => {
            let mut v = Vec::new();
            for i in 0..n {
                let x = ev.arg(a, i)?;
                ev.walk(x, false, &mut |val, _| {
                    match val {
                        Value::Number(x) => v.push(*x),
                        Value::Bool(b) => v.push(if *b { 1.0 } else { 0.0 }),
                        Value::Text(_) => v.push(0.0),
                        Value::Error(e) => return Err(*e),
                        Value::Empty => {}
                    }
                    Ok(())
                })?;
            }
            match name {
                "AVERAGEA" => {
                    if v.is_empty() { Err(E::Div0) } else { num(v.iter().sum::<f64>() / v.len() as f64) }
                }
                "MAXA" => num(v.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(if v.is_empty() { 0.0 } else { f64::NEG_INFINITY })),
                _ => num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::INFINITY, f64::min) }),
            }
        }
        "MAX" => {
            let v = ev.numbers(a, false)?;
            num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::NEG_INFINITY, f64::max) })
        }
        "MIN" => {
            let v = ev.numbers(a, false)?;
            num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::INFINITY, f64::min) })
        }
        "MEDIAN" => percentile(ev.numbers(a, false)?, 0.5).and_then(num),
        "MODE" | "MODE.SNGL" => {
            let v = ev.numbers(a, false)?;
            let mut best = None;
            let mut best_n = 1;
            for (i, x) in v.iter().enumerate() {
                let c = v.iter().filter(|y| *y == x).count();
                if c > best_n || (c == best_n && best.is_none() && c > 1) {
                    best_n = c;
                    best = Some(i);
                }
            }
            match best {
                Some(i) => num(v[i]),
                None => Err(E::NA),
            }
        }
        "STDEV" | "STDEV.S" => num(stats(&ev.numbers(a, false)?, true)?.sqrt()),
        "STDEVP" | "STDEV.P" => num(stats(&ev.numbers(a, false)?, false)?.sqrt()),
        "VAR" | "VAR.S" => num(stats(&ev.numbers(a, false)?, true)?),
        "VARP" | "VAR.P" => num(stats(&ev.numbers(a, false)?, false)?),
        "LARGE" | "SMALL" => {
            let mut v = ev.numbers(&a[..1.min(n)], false)?;
            let k = ev.number(a, 1)? as usize;
            if k == 0 || k > v.len() {
                return Err(E::Num);
            }
            v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(Ordering::Equal));
            if name == "LARGE" { num(v[v.len() - k]) } else { num(v[k - 1]) }
        }
        "PERCENTILE" | "PERCENTILE.INC" => {
            let v = ev.numbers(&a[..1.min(n)], false)?;
            num(percentile(v, ev.number(a, 1)?)?)
        }
        "QUARTILE" | "QUARTILE.INC" => {
            let v = ev.numbers(&a[..1.min(n)], false)?;
            let q = ev.number(a, 1)?.floor();
            if !(0.0..=4.0).contains(&q) {
                return Err(E::Num);
            }
            num(percentile(v, q / 4.0)?)
        }
        "RANK" | "RANK.EQ" => {
            let x = ev.number(a, 0)?;
            let v = ev.numbers(&a[1..2.min(n)], false)?;
            let desc = ev.number_or(a, 2, 0.0)? == 0.0;
            if !v.contains(&x) {
                return Err(E::NA);
            }
            let rank = v.iter().filter(|y| if desc { **y > x } else { **y < x }).count() + 1;
            num(rank as f64)
        }
        "CORREL" => {
            let x = ev.arg(a, 0)?;
            let y = ev.arg(a, 1)?;
            let mut gs = aligned_grids(ev, vec![x, y]).into_iter().map(|g| g.into_iter().flatten().collect::<Vec<Value>>());
            let (gx, gy) = (gs.next().unwrap_or_default(), gs.next().unwrap_or_default());
            let pairs: Vec<(f64, f64)> = gx
                .iter()
                .zip(gy.iter())
                .filter_map(|(a, b)| match (a, b) {
                    (Value::Number(x), Value::Number(y)) => Some((*x, *y)),
                    _ => None,
                })
                .collect();
            let nn = pairs.len() as f64;
            if nn < 2.0 {
                return Err(E::Div0);
            }
            let mx = pairs.iter().map(|p| p.0).sum::<f64>() / nn;
            let my = pairs.iter().map(|p| p.1).sum::<f64>() / nn;
            let cov: f64 = pairs.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
            let sx: f64 = pairs.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>().sqrt();
            let sy: f64 = pairs.iter().map(|p| (p.1 - my).powi(2)).sum::<f64>().sqrt();
            if sx == 0.0 || sy == 0.0 { Err(E::Div0) } else { num(cov / (sx * sy)) }
        }
        "COUNT" => {
            let mut c = 0.0;
            for i in 0..n {
                let Ok(v) = ev.arg(a, i) else { continue };
                let _ = ev.walk(v, false, &mut |val, from_ref| {
                    match val {
                        Value::Number(_) => c += 1.0,
                        Value::Bool(_) if !from_ref => c += 1.0,
                        Value::Text(s) if !from_ref && numfmt::parse_number(s).is_some() => c += 1.0,
                        _ => {}
                    }
                    Ok(())
                });
            }
            num(c)
        }
        "COUNTA" => {
            let mut c = 0.0;
            for i in 0..n {
                match ev.arg(a, i) {
                    Ok(v) => {
                        let _ = ev.walk(v, false, &mut |val, _| {
                            if !val.is_empty() {
                                c += 1.0;
                            }
                            Ok(())
                        });
                    }
                    Err(_) => c += 1.0,
                }
            }
            num(c)
        }
        "COUNTBLANK" => {
            let v = ev.arg(a, 0)?;
            let total = match &v {
                EV::R(_, rg) => rg.rows() as f64 * rg.cols() as f64,
                _ => 1.0,
            };
            let mut filled = 0.0;
            ev.walk(v, false, &mut |val, _| {
                if !val.is_empty() && !matches!(val, Value::Text(s) if s.is_empty()) {
                    filled += 1.0;
                }
                Ok(())
            })?;
            num(total - filled)
        }

        // ----- Logical -----
        "TRUE" => boolean(true),
        "FALSE" => boolean(false),
        "IF" => {
            need(a, 1, 3)?;
            let c = ev.boolean(a, 0)?;
            if c {
                if n >= 2 { ev.eval(&a[1]).map(|v| missing_zero(&a[1], v)) } else { boolean(true) }
            } else if n >= 3 {
                ev.eval(&a[2]).map(|v| missing_zero(&a[2], v))
            } else {
                boolean(false)
            }
        }
        "IFS" => {
            let mut i = 0;
            while i + 1 < n {
                if ev.boolean(a, i)? {
                    return ev.eval(&a[i + 1]);
                }
                i += 2;
            }
            Err(E::NA)
        }
        "IFERROR" | "IFNA" => {
            need(a, 2, 2)?;
            let only_na = name == "IFNA";
            let r = ev.eval(&a[0]);
            let failed = match &r {
                Err(e) => !only_na || *e == E::NA,
                Ok(v) if !ev.is_multi(v) => match ev.scalar(v.clone()) {
                    Err(e) => !only_na || e == E::NA,
                    Ok(_) => false,
                },
                Ok(_) => false,
            };
            if failed { ev.eval(&a[1]) } else { r }
        }
        "AND" | "OR" | "XOR" => {
            let mut vals = Vec::new();
            for i in 0..n {
                let v = ev.arg(a, i)?;
                ev.walk(v, false, &mut |val, from_ref| {
                    match val {
                        Value::Bool(b) => vals.push(*b),
                        Value::Number(x) => vals.push(*x != 0.0),
                        Value::Error(e) => return Err(*e),
                        Value::Text(_) if !from_ref => vals.push(to_bool(val)?),
                        _ => {}
                    }
                    Ok(())
                })?;
            }
            if vals.is_empty() {
                return Err(E::Value);
            }
            boolean(match name {
                "AND" => vals.iter().all(|b| *b),
                "OR" => vals.iter().any(|b| *b),
                _ => vals.iter().filter(|b| **b).count() % 2 == 1,
            })
        }
        "NOT" => boolean(!ev.boolean(a, 0)?),
        "SWITCH" => {
            if n < 3 {
                return Err(E::Value);
            }
            let key = ev.value(a, 0)?;
            let mut i = 1;
            while i + 1 < n {
                let v = ev.value(a, i)?;
                if values_equal(&key, &v) {
                    return ev.eval(&a[i + 1]);
                }
                i += 2;
            }
            if i < n { ev.eval(&a[i]) } else { Err(E::NA) }
        }

        // ----- Information -----
        "ISBLANK" => Ok(EV::V(Value::Bool(matches!(ev.value(a, 0), Ok(Value::Empty))))),
        "ISNUMBER" => boolean(matches!(ev.value(a, 0), Ok(Value::Number(_)))),
        "ISTEXT" => boolean(matches!(ev.value(a, 0), Ok(Value::Text(_)))),
        "ISNONTEXT" => boolean(!matches!(ev.value(a, 0), Ok(Value::Text(_)))),
        "ISLOGICAL" => boolean(matches!(ev.value(a, 0), Ok(Value::Bool(_)))),
        "ISERROR" => boolean(ev.value(a, 0).is_err()),
        "ISERR" => boolean(matches!(ev.value(a, 0), Err(e) if e != E::NA)),
        "ISNA" => boolean(matches!(ev.value(a, 0), Err(E::NA))),
        "ISEVEN" => boolean((ev.number(a, 0)?.trunc() as i64) % 2 == 0),
        "ISODD" => boolean((ev.number(a, 0)?.trunc() as i64) % 2 != 0),
        "ISFORMULA" => match ev.arg(a, 0)? {
            EV::R(s, rg) => boolean(ev.wb.sheets[s].get(rg.r1, rg.c1).is_some_and(|c| c.formula.is_some())),
            _ => Err(E::Value),
        },
        "NA" => Err(E::NA),
        "ERROR.TYPE" => match ev.value(a, 0) {
            Err(e) => num(e.code()),
            Ok(_) => Err(E::NA),
        },
        "TYPE" => num(match ev.value(a, 0) {
            Ok(Value::Number(_)) | Ok(Value::Empty) => 1.0,
            Ok(Value::Text(_)) => 2.0,
            Ok(Value::Bool(_)) => 4.0,
            Ok(Value::Error(_)) | Err(_) => 16.0,
        }),
        "N" => num(match ev.value(a, 0)? {
            Value::Number(x) => x,
            Value::Bool(b) => if b { 1.0 } else { 0.0 },
            _ => 0.0,
        }),
        "T" => text(match ev.value(a, 0)? {
            Value::Text(s) => s,
            _ => String::new(),
        }),

        // ----- Text -----
        "CONCATENATE" => {
            let mut s = String::new();
            for i in 0..n {
                s.push_str(&ev.string(a, i)?);
            }
            text(s)
        }
        "CONCAT" | "TEXTJOIN" => {
            let (start, delim, ignore) = if name == "TEXTJOIN" { (2, ev.string(a, 0)?, ev.boolean(a, 1)?) } else { (0, String::new(), false) };
            let mut parts = Vec::new();
            // Characters of the result so far: stop at Excel's text limit (#VALUE!) rather than
            // building huge strings (e.g. a long delimiter over a whole column).
            let (dlen, mut total) = (delim.chars().count(), 0usize);
            for i in start..n {
                let v = ev.arg(a, i)?;
                ev.walk(v, false, &mut |val, _| {
                    let t = to_text(val)?;
                    if !(ignore && t.is_empty()) {
                        total = total.saturating_add(t.chars().count() + if parts.is_empty() { 0 } else { dlen });
                        if total > MAX_TEXT {
                            return Err(E::Value);
                        }
                        parts.push(t);
                    }
                    Ok(())
                })?;
            }
            text(parts.join(&delim))
        }
        "LEFT" | "RIGHT" => {
            let s: Vec<char> = ev.string(a, 0)?.chars().collect();
            let k = ev.number_or(a, 1, 1.0)?;
            if k < 0.0 {
                return Err(E::Value);
            }
            let k = (k as usize).min(s.len());
            text(if name == "LEFT" { s[..k].iter().collect::<String>() } else { s[s.len() - k..].iter().collect() })
        }
        "MID" => {
            let s: Vec<char> = ev.string(a, 0)?.chars().collect();
            let st = ev.number(a, 1)?;
            let k = ev.number(a, 2)?;
            if st < 1.0 || k < 0.0 {
                return Err(E::Value);
            }
            // `as usize` saturates for huge counts; clamp before adding so nothing overflows.
            let st = ((st - 1.0) as usize).min(s.len());
            let end = st.saturating_add(k as usize).min(s.len());
            text(s[st..end].iter().collect::<String>())
        }
        "LEN" => num(ev.string(a, 0)?.chars().count() as f64),
        "UPPER" => text(ev.string(a, 0)?.to_uppercase()),
        "LOWER" => text(ev.string(a, 0)?.to_lowercase()),
        "PROPER" => {
            let s = ev.string(a, 0)?;
            let mut out = String::new();
            let mut prev_alpha = false;
            for ch in s.chars() {
                if prev_alpha { out.extend(ch.to_lowercase()) } else { out.extend(ch.to_uppercase()) }
                prev_alpha = ch.is_alphabetic();
            }
            text(out)
        }
        "TRIM" => text(ev.string(a, 0)?.split(' ').filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")),
        "CLEAN" => text(ev.string(a, 0)?.chars().filter(|c| (*c as u32) >= 32).collect::<String>()),
        "SUBSTITUTE" => {
            let s = ev.string(a, 0)?;
            let old = ev.string(a, 1)?;
            let new = ev.string(a, 2)?;
            if old.is_empty() {
                return text(s);
            }
            if ev.has(a, 3) {
                let inst = ev.number(a, 3)? as usize;
                if inst == 0 {
                    return Err(E::Value);
                }
                match s.match_indices(&old).nth(inst - 1) {
                    Some((i, _)) => text(format!("{}{}{}", &s[..i], new, &s[i + old.len()..])),
                    None => text(s),
                }
            } else {
                // Refuse results past Excel's text limit before building them.
                let (len, grow) = (s.chars().count(), new.chars().count().saturating_sub(old.chars().count()));
                if len.saturating_add(s.matches(&old).count().saturating_mul(grow)) > MAX_TEXT.max(len) {
                    return Err(E::Value);
                }
                text(s.replace(&old, &new))
            }
        }
        "REPLACE" => {
            let s: Vec<char> = ev.string(a, 0)?.chars().collect();
            let st = ((ev.number(a, 1)?.max(1.0) - 1.0) as usize).min(s.len());
            let k = ev.number(a, 2)?.max(0.0) as usize;
            let new = ev.string(a, 3)?;
            let end = st.saturating_add(k).min(s.len());
            text(format!("{}{}{}", s[..st].iter().collect::<String>(), new, s[end..].iter().collect::<String>()))
        }
        "FIND" | "SEARCH" => {
            let f = ev.string(a, 0)?;
            let s = ev.string(a, 1)?;
            let st = ev.number_or(a, 2, 1.0)?.trunc();
            let sc: Vec<char> = s.chars().collect();
            if !(1.0..=sc.len() as f64 + 1.0).contains(&st) {
                return Err(E::Value);
            }
            let st = st as usize - 1;
            if name == "FIND" {
                let hay: String = sc[st..].iter().collect();
                return match hay.find(&f) {
                    Some(b) => num((st + hay[..b].chars().count() + 1) as f64),
                    None => Err(E::Value),
                };
            }
            let pc: Vec<char> = f.chars().collect();
            if !has_wild(&f) {
                let hay: String = sc[st..].iter().collect::<String>().to_lowercase();
                return match hay.find(&f.to_lowercase()) {
                    Some(b) => num((st + hay[..b].chars().count() + 1) as f64),
                    None => Err(E::Value),
                };
            }
            // The pattern matches some sc[i..j] exactly when "pattern*" matches sc[i..]: one
            // match per start position instead of one per (start, end) pair.
            let prefix = search_prefix_pattern(&pc);
            for i in st..=sc.len() {
                if wild_match(&prefix, &sc[i..]) {
                    return num((i + 1) as f64);
                }
            }
            Err(E::Value)
        }
        "REPT" => {
            let s = ev.string(a, 0)?;
            let k = ev.number(a, 1)?.trunc();
            if k < 0.0 || s.chars().count() as f64 * k > MAX_TEXT as f64 {
                return Err(E::Value);
            }
            text(if s.is_empty() { String::new() } else { s.repeat(k as usize) })
        }
        "EXACT" => boolean(ev.string(a, 0)? == ev.string(a, 1)?),
        "CHAR" | "UNICHAR" => {
            let k = ev.number(a, 0)? as u32;
            match char::from_u32(k) {
                Some(c) if k >= 1 => text(c.to_string()),
                _ => Err(E::Value),
            }
        }
        "CODE" | "UNICODE" => match ev.string(a, 0)?.chars().next() {
            Some(c) => num(c as u32 as f64),
            None => Err(E::Value),
        },
        "VALUE" => num(to_num(&Value::Text(ev.string(a, 0)?))?),
        "TEXT" => {
            let mut v = ev.value(a, 0)?;
            if let Value::Text(s) = &v {
                if let Value::Number(x) = numfmt::parse_input(s).0 {
                    v = Value::Number(x);
                }
            }
            let f = ev.string(a, 1)?;
            text(numfmt::format_value(&v, &f).text)
        }
        "FIXED" => {
            let x = ev.number(a, 0)?;
            let d = ev.number_or(a, 1, 2.0)?.trunc();
            if d > 127.0 {
                return Err(E::Value);
            }
            let d = d.max(0.0) as usize;
            let no_commas = ev.bool_or(a, 2, false)?;
            let fmt = format!("{}{}{}", if no_commas { "0" } else { "#,##0" }, if d > 0 { "." } else { "" }, "0".repeat(d));
            text(numfmt::format_value(&Value::Number(round_half_away(x, d as i32)), &fmt).text)
        }
        "TEXTBEFORE" | "TEXTAFTER" => {
            let s = ev.string(a, 0)?;
            let d = ev.string(a, 1)?;
            let inst = ev.number_or(a, 2, 1.0)?.trunc();
            if d.is_empty() || inst == 0.0 {
                return Err(E::Value);
            }
            let idx: Vec<usize> = s.match_indices(&d).map(|(i, _)| i).collect();
            // Float -> usize casts saturate, so huge or very negative instances stay in range.
            let pick = if inst > 0.0 { idx.get(inst as usize - 1) } else { idx.len().checked_sub((-inst) as usize).and_then(|k| idx.get(k)) };
            match pick {
                Some(&i) => text(if name == "TEXTBEFORE" { s[..i].to_string() } else { s[i + d.len()..].to_string() }),
                None => Err(E::NA),
            }
        }

        // ----- Date & time -----
        "TODAY" => num(ev.now.floor()),
        "NOW" => num(ev.now),
        "DATE" => {
            let y = ev.number(a, 0)?.trunc();
            let m = ev.number(a, 1)?.trunc();
            let d = ev.number(a, 2)?.trunc();
            // Bound the parts before the integer date arithmetic (results past 9999 are #NUM!).
            if !(0.0..=9999.0).contains(&y) || m.abs() > 1e6 || d.abs() > 1e8 {
                return Err(E::Num);
            }
            let s = numfmt::ymd_to_serial(y as i64, m as i64, d as i64);
            num(serial(s)?)
        }
        "TIME" => {
            let h = ev.number(a, 0)?;
            let m = ev.number(a, 1)?;
            let s = ev.number(a, 2)?;
            let t = (h * 3600.0 + m * 60.0 + s) / 86400.0;
            if t < 0.0 { Err(E::Num) } else { num(t.fract()) }
        }
        "YEAR" | "MONTH" | "DAY" => {
            let s = serial(ev.number(a, 0)?)?;
            let (y, m, d) = numfmt::serial_to_ymd(s);
            num(match name {
                "YEAR" => y,
                "MONTH" => m,
                _ => d,
            } as f64)
        }
        "HOUR" | "MINUTE" | "SECOND" => {
            let s = ev.number(a, 0)?;
            let (h, m, sec, _) = numfmt::serial_to_hms(s);
            num(match name {
                "HOUR" => h,
                "MINUTE" => m,
                _ => sec,
            } as f64)
        }
        "DATEVALUE" => match datevalue(&ev.string(a, 0)?) {
            Some(s) => num(s),
            None => Err(E::Value),
        },
        "TIMEVALUE" => match numfmt::parse_time(&ev.string(a, 0)?) {
            Some((s, _)) => num(s),
            None => Err(E::Value),
        },
        "WEEKDAY" => {
            let s = serial(ev.number(a, 0)?)?;
            let t = ev.number_or(a, 1, 1.0)? as i64;
            let w = numfmt::weekday(s); // 1=Sun
            num(match t {
                1 | 17 => w,
                2 | 11 => (w + 5) % 7 + 1,
                3 => (w + 5) % 7,
                12..=16 => (w - (t - 9)).rem_euclid(7) + 1,
                _ => return Err(E::Num),
            } as f64)
        }
        "WEEKNUM" => {
            let s = serial(ev.number(a, 0)?)?.floor();
            let t = ev.number_or(a, 1, 1.0)? as i64;
            let (y, _, _) = numfmt::serial_to_ymd(s);
            let jan1 = numfmt::ymd_to_serial(y, 1, 1);
            let start_dow = if t == 2 || t == 11 { 2 } else { 1 };
            let offset = (numfmt::weekday(jan1) - start_dow).rem_euclid(7);
            num(((s - jan1 + offset as f64) / 7.0).floor() + 1.0)
        }
        "EDATE" | "EOMONTH" => {
            let s = serial(ev.number(a, 0)?)?;
            let m = ev.number(a, 1)?.trunc();
            // 120,000 months spans the whole date range; beyond is #NUM! (as is any result).
            if m.abs() > 120_000.0 {
                return Err(E::Num);
            }
            num(serial(add_months(s, m as i64, name == "EOMONTH"))?)
        }
        "DAYS" => num(ev.number(a, 0)?.floor() - ev.number(a, 1)?.floor()),
        "DATEDIF" => {
            let s = serial(ev.number(a, 0)?)?.floor();
            let e = serial(ev.number(a, 1)?)?.floor();
            let unit = ev.string(a, 2)?.to_uppercase();
            if s > e {
                return Err(E::Num);
            }
            let (y1, m1, d1) = numfmt::serial_to_ymd(s);
            let (y2, m2, d2) = numfmt::serial_to_ymd(e);
            let mut months = (y2 - y1) * 12 + (m2 - m1);
            if d2 < d1 {
                months -= 1;
            }
            num(match unit.as_str() {
                "Y" => (months / 12) as f64,
                "M" => months as f64,
                "D" => e - s,
                "YM" => (months % 12) as f64,
                "MD" => {
                    if d2 >= d1 {
                        (d2 - d1) as f64
                    } else {
                        let (py, pm) = if m2 == 1 { (y2 - 1, 12) } else { (y2, m2 - 1) };
                        (numfmt::days_in_month(py, pm) - d1 + d2) as f64
                    }
                }
                "YD" => {
                    let anchor = numfmt::ymd_to_serial(if (m2, d2) >= (m1, d1) { y2 } else { y2 - 1 }, m1, d1);
                    e - anchor
                }
                _ => return Err(E::Num),
            })
        }
        "NETWORKDAYS" | "WORKDAY" => {
            let s = serial(ev.number(a, 0)?)?.floor();
            let mut holidays = Default::default();
            if ev.has(a, 2) {
                holidays = holiday_set(ev.numbers(&a[2..3], false)?);
            }
            let is_off = |d: f64| is_weekend(d) || holidays.contains(&(d as i64));
            if name == "NETWORKDAYS" {
                let e = serial(ev.number(a, 1)?)?.floor();
                let (lo, hi, sign) = if s <= e { (s, e, 1.0) } else { (e, s, -1.0) };
                if hi - lo > WORKDAYS_MAX {
                    return Err(E::Num);
                }
                let mut c = 0.0;
                let mut d = lo;
                while d <= hi {
                    if !is_off(d) {
                        c += 1.0;
                    }
                    d += 1.0;
                }
                num(c * sign)
            } else {
                // Each step is bounded: at most WORKDAYS_MAX working days, and the walk stops
                // once it leaves the date range.
                let days = ev.number(a, 1)?.trunc();
                if days.abs() > WORKDAYS_MAX {
                    return Err(E::Num);
                }
                let step = if days >= 0.0 { 1.0 } else { -1.0 };
                let mut left = days.abs();
                let mut d = s;
                while left > 0.0 {
                    d += step;
                    serial(d)?;
                    if !is_off(d) {
                        left -= 1.0;
                    }
                }
                num(d)
            }
        }

        // ----- Financial -----
        "PMT" => {
            let r = ev.number(a, 0)?;
            let np = ev.number(a, 1)?;
            let pv = ev.number(a, 2)?;
            let f = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            if np == 0.0 { Err(E::Num) } else { num(pmt(r, np, pv, f, t)) }
        }
        "IPMT" | "PPMT" => {
            let r = ev.number(a, 0)?;
            let per = ev.number(a, 1)?;
            let np = ev.number(a, 2)?;
            let pv = ev.number(a, 3)?;
            let f = ev.number_or(a, 4, 0.0)?;
            let t = ev.number_or(a, 5, 0.0)?;
            if per < 1.0 || per > np {
                return Err(E::Num);
            }
            let i = ipmt(r, per, np, pv, f, t);
            if name == "IPMT" { num(i) } else { num(pmt(r, np, pv, f, t) - i) }
        }
        "FV" => {
            let r = ev.number(a, 0)?;
            let np = ev.number(a, 1)?;
            let p = ev.number(a, 2)?;
            let pv = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            num(fv(r, np, p, pv, t))
        }
        "PV" => {
            let r = ev.number(a, 0)?;
            let np = ev.number(a, 1)?;
            let p = ev.number(a, 2)?;
            let f = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            if r == 0.0 {
                return num(-(f + p * np));
            }
            let q = (1.0 + r).powf(np);
            num(-(f + p * (1.0 + r * t) * (q - 1.0) / r) / q)
        }
        "NPER" => {
            let r = ev.number(a, 0)?;
            let p = ev.number(a, 1)?;
            let pv = ev.number(a, 2)?;
            let f = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            if r == 0.0 {
                if p == 0.0 {
                    return Err(E::Num);
                }
                return num(-(pv + f) / p);
            }
            let z = p * (1.0 + r * t) / r;
            let v = ((z - f) / (pv + z)).ln() / (1.0 + r).ln();
            num(v)
        }
        "NPV" => {
            let r = ev.number(a, 0)?;
            let vals = ev.numbers(&a[1.min(n)..], false)?;
            num(vals.iter().enumerate().map(|(i, v)| v / (1.0 + r).powi(i as i32 + 1)).sum())
        }
        "IRR" => {
            let vals = ev.numbers(&a[..1.min(n)], false)?;
            let mut r = ev.number_or(a, 1, 0.1)?;
            for _ in 0..100 {
                let (mut f, mut df) = (0.0, 0.0);
                for (i, v) in vals.iter().enumerate() {
                    let d = (1.0 + r).powi(i as i32);
                    f += v / d;
                    df -= i as f64 * v / (d * (1.0 + r));
                }
                if df == 0.0 {
                    break;
                }
                let nr = r - f / df;
                if (nr - r).abs() < 1e-10 {
                    return num(nr);
                }
                r = nr;
            }
            Err(E::Num)
        }
        "RATE" => {
            let np = ev.number(a, 0)?;
            let p = ev.number(a, 1)?;
            let pv = ev.number(a, 2)?;
            let f = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            let mut r = ev.number_or(a, 5, 0.1)?;
            let g = |r: f64| f - fv(r, np, p, pv, t);
            for _ in 0..100 {
                let y = g(r);
                let h = 1e-7;
                let dy = (g(r + h) - y) / h;
                if dy == 0.0 {
                    break;
                }
                let nr = r - y / dy;
                if (nr - r).abs() < 1e-10 {
                    return num(nr);
                }
                r = nr;
            }
            Err(E::Num)
        }

        // ----- Lookup & reference -----
        "ROW" | "COLUMN" => {
            if n == 0 || !ev.has(a, 0) {
                let (_, r, c) = ev.cur;
                return num(if name == "ROW" { r + 1 } else { c + 1 } as f64);
            }
            match ev.arg(a, 0)? {
                EV::R(s, rg) if !rg.is_single() => {
                    let rg = ev.clip(s, rg);
                    Ok(EV::A(if name == "ROW" {
                        (rg.r1..=rg.r2).map(|r| vec![Value::Number(r as f64 + 1.0)]).collect()
                    } else {
                        vec![(rg.c1..=rg.c2).map(|c| Value::Number(c as f64 + 1.0)).collect()]
                    }))
                }
                EV::R(_, rg) => num(if name == "ROW" { rg.r1 + 1 } else { rg.c1 + 1 } as f64),
                _ => Err(E::Value),
            }
        }
        "ROWS" | "COLUMNS" => match ev.arg(a, 0)? {
            EV::R(_, rg) => num(if name == "ROWS" { rg.rows() } else { rg.cols() } as f64),
            EV::A(g) => num(if name == "ROWS" { g.len() } else { g.first().map_or(0, |r| r.len()) } as f64),
            EV::V(_) => num(1.0),
            _ => Err(E::Value),
        },
        "CHOOSE" => {
            let i = ev.number(a, 0)?.trunc() as usize;
            if i < 1 || i >= n {
                return Err(E::Value);
            }
            ev.eval(&a[i])
        }
        "INDEX" => {
            need(a, 2, 4)?;
            let v = ev.arg(a, 0)?;
            let mut r = ev.number_or(a, 1, 0.0)?.trunc() as i64;
            let mut c = ev.number_or(a, 2, 0.0)?.trunc() as i64;
            // area_num picks one area of a multi-area reference `(A1:B2,D1:E2)`.
            let area = ev.number_or(a, 3, 1.0)?.trunc() as i64;
            let v = match v {
                EV::M(m) => {
                    let (s, rg) = *m.get((area - 1).max(0) as usize).filter(|_| area >= 1).ok_or(E::Ref)?;
                    EV::R(s, rg)
                }
                _ if area != 1 => return Err(E::Ref),
                v => v,
            };
            match v {
                EV::R(s, rg) => {
                    if rg.rows() == 1 && !ev.has(a, 2) {
                        c = r;
                        r = 1;
                    }
                    if r < 0 || c < 0 || r > rg.rows() as i64 || c > rg.cols() as i64 {
                        return Err(E::Ref);
                    }
                    let (r1, r2) = if r == 0 { (rg.r1, rg.r2) } else { (rg.r1 + r as u32 - 1, rg.r1 + r as u32 - 1) };
                    let (c1, c2) = if c == 0 { (rg.c1, rg.c2) } else { (rg.c1 + c as u32 - 1, rg.c1 + c as u32 - 1) };
                    Ok(EV::R(s, Range::new(r1, c1, r2, c2)))
                }
                other => {
                    let g = ev.grid(other);
                    if g.len() == 1 && !ev.has(a, 2) {
                        c = r;
                        r = 1;
                    }
                    let r = if r == 0 { 1 } else { r };
                    let c = if c == 0 { 1 } else { c };
                    g.get(r as usize - 1).and_then(|row| row.get(c as usize - 1)).cloned().map(EV::V).ok_or(E::Ref)
                }
            }
        }
        "OFFSET" => {
            need(a, 3, 5)?;
            let EV::R(s, rg) = ev.arg(a, 0)? else { return Err(E::Value) };
            let dr = ev.number(a, 1)?.trunc();
            let dc = ev.number(a, 2)?.trunc();
            let h = if ev.has(a, 3) { ev.number(a, 3)?.trunc() } else { rg.rows() as f64 };
            let w = if ev.has(a, 4) { ev.number(a, 4)?.trunc() } else { rg.cols() as f64 };
            // Check the target against the sheet in floating point before any integer
            // arithmetic, so huge offsets or sizes cannot overflow.
            let r1 = rg.r1 as f64 + dr;
            let c1 = rg.c1 as f64 + dc;
            if r1 < 0.0 || c1 < 0.0 || h < 1.0 || w < 1.0 || r1 + h > MAX_ROWS as f64 || c1 + w > MAX_COLS as f64 {
                return Err(E::Ref);
            }
            let (r1, c1) = (r1 as u32, c1 as u32);
            Ok(EV::R(s, Range::new(r1, c1, r1 + h as u32 - 1, c1 + w as u32 - 1)))
        }
        "INDIRECT" => {
            let t = ev.string(a, 0)?;
            match super::parse(&t) {
                Ok(Expr::Ref(r)) => {
                    let (s, rg) = ev.resolve_ref(&r)?;
                    let _ = RefKind::Cell;
                    Ok(EV::R(s, rg))
                }
                _ => Err(E::Ref),
            }
        }
        "VLOOKUP" | "HLOOKUP" => {
            need(a, 3, 4)?;
            let key = ev.value(a, 0)?;
            let tv = ev.arg(a, 1)?;
            if let EV::R(s, rg) = tv {
                let vert = name == "VLOOKUP";
                let idx = ev.number(a, 2)?.trunc() as i64;
                if idx < 1 {
                    return Err(E::Value);
                }
                if idx > (if vert { rg.cols() } else { rg.rows() }) as i64 {
                    return Err(E::Ref);
                }
                let approx = ev.bool_or(a, 3, true)?;
                let keys = if vert { Range::new(rg.r1, rg.c1, rg.r2, rg.c1) } else { Range::new(rg.r1, rg.c1, rg.r1, rg.c2) };
                let col = ev.lookup_col(s, keys);
                let hit = if approx { lookup_approx(&col.vals, &key) } else { find_exact(&col, &key, true, false) };
                let i = hit.ok_or(E::NA)? as u32;
                let off = idx as u32 - 1;
                let (r, c) = if vert { (rg.r1 + i, rg.c1 + off) } else { (rg.r1 + off, rg.c1 + i) };
                return Ok(EV::V(ev.cell_value(s, r, c)));
            }
            let full = match &tv {
                EV::R(_, rg) => Some(*rg),
                _ => None,
            };
            let mut g = ev.grid(tv);
            if name == "HLOOKUP" {
                g = transpose(g);
            }
            let idx = ev.number(a, 2)?.trunc() as i64;
            let width = match full {
                Some(rg) => (if name == "VLOOKUP" { rg.cols() } else { rg.rows() }) as i64,
                None => g.first().map_or(0, |r| r.len()) as i64,
            };
            if idx < 1 {
                return Err(E::Value);
            }
            if idx > width {
                return Err(E::Ref);
            }
            let approx = ev.bool_or(a, 3, true)?;
            let keys: Vec<Value> = g.iter().map(|r| r.first().cloned().unwrap_or_default()).collect();
            let hit = if approx { lookup_approx(&keys, &key) } else { lookup_exact(&keys, &key, true, false) };
            match hit {
                Some(i) => Ok(EV::V(get(&g, i, idx as usize - 1))),
                None => Err(E::NA),
            }
        }
        "MATCH" => {
            need(a, 2, 3)?;
            let key = ev.value(a, 0)?;
            let v = ev.arg(a, 1)?;
            let col = lookup_vector(ev, v);
            let vals = &col.vals;
            let t = ev.number_or(a, 2, 1.0)? as i64;
            let hit = match t {
                0 => find_exact(&col, &key, true, false),
                1 => lookup_approx(vals, &key),
                _ => {
                    let mut best = None;
                    for (i, v) in vals.iter().enumerate() {
                        if std::mem::discriminant(v) != std::mem::discriminant(&key) {
                            continue;
                        }
                        if compare(v, &key) != Ordering::Less {
                            best = Some(i);
                        } else {
                            break;
                        }
                    }
                    best
                }
            };
            hit.map(|i| EV::V(Value::Number(i as f64 + 1.0))).ok_or(E::NA)
        }
        "XMATCH" => {
            let key = ev.value(a, 0)?;
            let v = ev.arg(a, 1)?;
            let col = lookup_vector(ev, v);
            let mode = ev.number_or(a, 2, 0.0)? as i64;
            let rev = ev.number_or(a, 3, 1.0)? < 0.0;
            find_exact(&col, &key, mode == 2, rev)
                .or_else(|| lookup_mode(&col.vals, &key, mode, rev))
                .map(|i| EV::V(Value::Number(i as f64 + 1.0)))
                .ok_or(E::NA)
        }
        "XLOOKUP" => {
            need(a, 3, 6)?;
            let key = ev.value(a, 0)?;
            let lv = ev.arg(a, 1)?;
            let vertical = match &lv {
                EV::R(_, rg) => rg.rows() > 1 || rg.cols() == 1,
                EV::A(g) => g.len() > 1 || g.first().map_or(0, |r| r.len()) == 1,
                _ => true,
            };
            let col = match lv {
                EV::R(s, rg) => ev.lookup_col(s, if vertical { Range::new(rg.r1, rg.c1, rg.r2, rg.c1) } else { Range::new(rg.r1, rg.c1, rg.r1, rg.c2) }),
                other => {
                    let lg = ev.grid(other);
                    Rc::new(LookupCol::new(if vertical { lg.iter().map(|r| r.first().cloned().unwrap_or_default()).collect() } else { lg.first().cloned().unwrap_or_default() }))
                }
            };
            let mode = ev.number_or(a, 4, 0.0)? as i64;
            let rev = ev.number_or(a, 5, 1.0)? < 0.0;
            let hit = if mode == 0 || mode == 2 { find_exact(&col, &key, mode == 2, rev) } else { lookup_mode(&col.vals, &key, mode, rev) };
            match hit {
                Some(i) => match ev.arg(a, 2)? {
                    EV::R(s, rg) => {
                        // A return range shorter than the hit position does not match in size.
                        if i >= if vertical { rg.rows() } else { rg.cols() } as usize {
                            return Err(E::Value);
                        }
                        let i = i as u32;
                        Ok(EV::R(s, if vertical { Range::new(rg.r1 + i, rg.c1, rg.r1 + i, rg.c2) } else { Range::new(rg.r1, rg.c1 + i, rg.r2, rg.c1 + i) }))
                    }
                    other => {
                        let g = ev.grid(other);
                        Ok(EV::A(if vertical { vec![g.get(i).cloned().ok_or(E::NA)?] } else { g.iter().map(|r| vec![r.get(i).cloned().unwrap_or_default()]).collect() }))
                    }
                },
                None => {
                    if ev.has(a, 3) { ev.eval(&a[3]) } else { Err(E::NA) }
                }
            }
        }
        "TRANSPOSE" => {
            let v = ev.arg(a, 0)?;
            let g = ev.grid(v);
            Ok(EV::A(transpose(g)))
        }
        "SORT" => {
            let v = ev.arg(a, 0)?;
            let g = ev.grid(v);
            let idx = ev.number_or(a, 1, 1.0)? as usize;
            let desc = ev.number_or(a, 2, 1.0)? < 0.0;
            let by_col = ev.bool_or(a, 3, false)?;
            if idx < 1 {
                return Err(E::Value);
            }
            let out = if by_col { transpose(sort_grid(transpose(g), idx - 1, desc)) } else { sort_grid(g, idx - 1, desc) };
            Ok(EV::A(out))
        }
        "UNIQUE" => {
            let v = ev.arg(a, 0)?;
            let g = ev.grid(v);
            let once = ev.bool_or(a, 2, false)?;
            let key = |r: &Vec<Value>| r.iter().map(|v| numfmt::value_to_text(v).to_lowercase()).collect::<Vec<_>>().join("\u{1}");
            let mut out: Vec<Vec<Value>> = Vec::new();
            let mut seen: Vec<String> = Vec::new();
            for row in &g {
                let k = key(row);
                if once {
                    if g.iter().filter(|r| key(r) == k).count() == 1 {
                        out.push(row.clone());
                    }
                } else if !seen.contains(&k) {
                    seen.push(k);
                    out.push(row.clone());
                }
            }
            if out.is_empty() { Err(E::Calc) } else { Ok(EV::A(out)) }
        }
        "FILTER" => {
            let v = ev.arg(a, 0)?;
            let g = ev.grid(v);
            let m = ev.arg(a, 1)?;
            let mg = ev.grid(m);
            let mut out = Vec::new();
            for (i, row) in g.iter().enumerate() {
                let keep = match mg.get(i).and_then(|r| r.first()) {
                    Some(v) => to_bool(v).unwrap_or(false),
                    None => false,
                };
                if keep {
                    out.push(row.clone());
                }
            }
            if out.is_empty() {
                if ev.has(a, 2) { ev.eval(&a[2]) } else { Err(E::Calc) }
            } else {
                Ok(EV::A(out))
            }
        }
        "SINGLE" => match ev.arg(a, 0)? {
            EV::R(s, rg) if !rg.is_single() => match ev.implicit(s, rg)? {
                Value::Error(e) => Err(e),
                v => Ok(EV::V(v)),
            },
            EV::A(g) => Ok(EV::V(g.into_iter().next().and_then(|r| r.into_iter().next()).unwrap_or_default())),
            other => Ok(other),
        },
        _ => {
            let _ = is_ref;
            super::ext::call(ev, name, a)
        }
    }
}

/// DATEVALUE text parsing: dates with optional weekday prefix and time suffix, ISO date-times,
/// and Excel's fictitious 29-Feb-1900 (serial 60).
fn datevalue(s: &str) -> Option<f64> {
    let mut t = s.trim();
    if let Some((d, _)) = t.split_once('T').filter(|(d, _)| d.len() == 10 && d.matches('-').count() == 2) {
        t = d;
    }
    const DAYS: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    if let Some((w, rest)) = t.split_once([',', ' ']) {
        let w = w.to_ascii_lowercase();
        if w.len() >= 3 && DAYS.iter().any(|d| d.starts_with(&w)) {
            t = rest.trim_start_matches([',', ' ']);
        }
    }
    if let Some((d, _)) = numfmt::parse_date(t) {
        return Some(d);
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    for k in (1..words.len()).rev() {
        if numfmt::parse_time(&words[k..].join(" ")).is_some() {
            if let Some((d, _)) = numfmt::parse_date(&words[..k].join(" ")) {
                return Some(d);
            }
        }
    }
    match numfmt::parse_date(&t.replacen("29", "28", 1)) {
        Some((d, _)) if d == 59.0 && t.contains("29") => Some(60.0),
        _ => None,
    }
}

fn missing_zero(e: &Expr, v: EV) -> EV {
    if matches!(e, Expr::Missing) { EV::V(Value::Number(0.0)) } else { v }
}

fn subtotal(f: i64, v: Vec<f64>) -> Res {
    let cnt = v.len() as f64;
    match f {
        1 => if cnt == 0.0 { Err(E::Div0) } else { num(v.iter().sum::<f64>() / cnt) },
        2 | 3 => num(cnt),
        4 => num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::NEG_INFINITY, f64::max) }),
        5 => num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::INFINITY, f64::min) }),
        6 => num(v.iter().product()),
        7 => num(stats(&v, true)?.sqrt()),
        8 => num(stats(&v, false)?.sqrt()),
        9 => num(v.iter().sum()),
        10 => num(stats(&v, true)?),
        11 => num(stats(&v, false)?),
        _ => Err(E::Value),
    }
}

#[cfg(test)]
mod tests {
    use crate::formula::eval_formula;
    use crate::model::*;

    fn wb() -> Workbook {
        let mut wb = Workbook::new();
        let data = [
            ["Name", "Qty", "Price"],
            ["Apple", "3", "1.5"],
            ["Banana", "5", "0.25"],
            ["Cherry", "7", "4"],
            ["apple", "2", "1.5"],
        ];
        for (r, row) in data.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let cell = cell_from_input(&mut wb, v, 0);
                wb.sheets[0].set(r as u32, c as u32, cell);
            }
        }
        let cell = cell_from_input(&mut wb, "=B2*C2", 0);
        wb.sheets[0].set(1, 3, cell);
        wb.recalc();
        wb
    }

    fn ev(wb: &Workbook, f: &str) -> Value {
        eval_formula(wb, 0, 10, 10, f)
    }

    #[test]
    fn basics() {
        let w = wb();
        assert_eq!(ev(&w, "=SUM(B2:B5)"), Value::Number(17.0));
        assert_eq!(ev(&w, "=SUM(B:B)"), Value::Number(17.0));
        assert_eq!(ev(&w, "=AVERAGE(B2:B5)"), Value::Number(4.25));
        assert_eq!(ev(&w, "=D2"), Value::Number(4.5));
        assert_eq!(ev(&w, "=1+2*3"), Value::Number(7.0));
        assert_eq!(ev(&w, "=-2^2"), Value::Number(4.0));
        assert_eq!(ev(&w, "=2^3^2"), Value::Number(64.0));
        assert_eq!(ev(&w, "=10%"), Value::Number(0.1));
        assert_eq!(ev(&w, "=\"a\"&1&TRUE"), Value::Text("a1TRUE".into()));
        assert_eq!(ev(&w, "=1/0"), Value::Error(ErrorKind::Div0));
        assert_eq!(ev(&w, "=A2=\"APPLE\""), Value::Bool(true));
        assert_eq!(ev(&w, "=FOO(1)"), Value::Error(ErrorKind::Name));
    }

    #[test]
    fn lookups() {
        let w = wb();
        assert_eq!(ev(&w, "=VLOOKUP(\"Cherry\",A2:C5,3,FALSE)"), Value::Number(4.0));
        assert_eq!(ev(&w, "=VLOOKUP(\"Ch*\",A2:C5,2,FALSE)"), Value::Number(7.0));
        assert_eq!(ev(&w, "=VLOOKUP(\"Zed\",A2:C5,2,FALSE)"), Value::Error(ErrorKind::NA));
        assert_eq!(ev(&w, "=INDEX(B2:B5,MATCH(\"Banana\",A2:A5,0))"), Value::Number(5.0));
        assert_eq!(ev(&w, "=XLOOKUP(\"Banana\",A2:A5,C2:C5)"), Value::Number(0.25));
        assert_eq!(ev(&w, "=XLOOKUP(\"x\",A2:A5,C2:C5,\"none\")"), Value::Text("none".into()));
        assert_eq!(ev(&w, "=SUM(OFFSET(B2,1,0,2,1))"), Value::Number(12.0));
        assert_eq!(ev(&w, "=SUM(INDIRECT(\"B2:B3\"))"), Value::Number(8.0));
        assert_eq!(ev(&w, "=ROWS(A1:C5)*COLUMNS(A1:C5)"), Value::Number(15.0));
    }

    #[test]
    fn conditional() {
        let w = wb();
        assert_eq!(ev(&w, "=SUMIF(A2:A5,\"apple\",B2:B5)"), Value::Number(5.0));
        assert_eq!(ev(&w, "=COUNTIF(B2:B5,\">3\")"), Value::Number(2.0));
        assert_eq!(ev(&w, "=SUMIFS(B2:B5,A2:A5,\"a*\",C2:C5,1.5)"), Value::Number(5.0));
        assert_eq!(ev(&w, "=COUNTIFS(A2:A5,\"<>Banana\")"), Value::Number(3.0));
        assert_eq!(ev(&w, "=SUMPRODUCT(B2:B5,C2:C5)"), Value::Number(3.0 * 1.5 + 5.0 * 0.25 + 28.0 + 3.0));
        assert_eq!(ev(&w, "=SUMPRODUCT((B2:B5>3)*C2:C5)"), Value::Number(4.25));
        assert_eq!(ev(&w, "=IF(B2>2,\"y\",\"n\")"), Value::Text("y".into()));
        assert_eq!(ev(&w, "=IFERROR(1/0,\"oops\")"), Value::Text("oops".into()));
        assert_eq!(ev(&w, "=AND(TRUE,B2>1)"), Value::Bool(true));
        assert_eq!(ev(&w, "=COUNTA(A1:C5)"), Value::Number(15.0));
        assert_eq!(ev(&w, "=COUNT(A1:C5)"), Value::Number(8.0));
        assert_eq!(ev(&w, "=COUNTBLANK(A1:A7)"), Value::Number(2.0));
    }

    #[test]
    fn text_and_dates() {
        let w = wb();
        assert_eq!(ev(&w, "=LEFT(\"Hello\",2)&RIGHT(\"World\")"), Value::Text("Hed".into()));
        assert_eq!(ev(&w, "=MID(\"spreadsheet\",7,5)"), Value::Text("sheet".into()));
        assert_eq!(ev(&w, "=PROPER(\"hello wORLD\")"), Value::Text("Hello World".into()));
        assert_eq!(ev(&w, "=TRIM(\"  a   b \")"), Value::Text("a b".into()));
        assert_eq!(ev(&w, "=SEARCH(\"s*t\",\"spreadsheet\")"), Value::Number(1.0));
        assert_eq!(ev(&w, "=TEXTJOIN(\", \",TRUE,A2:A4)"), Value::Text("Apple, Banana, Cherry".into()));
        assert_eq!(ev(&w, "=TEXT(1234.5,\"#,##0.00\")"), Value::Text("1,234.50".into()));
        assert_eq!(ev(&w, "=DATE(2026,11,4)"), Value::Number(46330.0));
        assert_eq!(ev(&w, "=YEAR(46330)&\"-\"&MONTH(46330)"), Value::Text("2026-11".into()));
        assert_eq!(ev(&w, "=EOMONTH(DATE(2024,1,15),1)"), Value::Number(crate::numfmt::ymd_to_serial(2024, 2, 29)));
        assert_eq!(ev(&w, "=DATEDIF(DATE(2020,1,1),DATE(2026,11,4),\"Y\")"), Value::Number(6.0));
        assert_eq!(ev(&w, "=WEEKDAY(DATE(2026,11,4))"), Value::Number(4.0));
        assert_eq!(ev(&w, "=ROUND(2.5,0)+ROUND(1.005,2)"), Value::Number(4.01));
        assert_eq!(ev(&w, "=MOD(-3,2)"), Value::Number(1.0));
        assert_eq!(ev(&w, "=ROUND(PMT(0.05/12,360,200000),2)"), Value::Number(-1073.64));
    }

    #[test]
    fn circular() {
        let mut w = Workbook::new();
        let c = cell_from_input(&mut w, "=A2", 0);
        w.sheets[0].set(0, 0, c);
        let c = cell_from_input(&mut w, "=A1", 0);
        w.sheets[0].set(1, 0, c);
        w.recalc();
        assert!(w.circular);
    }
}
