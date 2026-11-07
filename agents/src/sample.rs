//! Small sample workbooks for the shared test instance.

use rust_xlsxwriter::Workbook;

/// Names of the samples `make` knows.
pub const SAMPLES: [&str; 3] = ["budget", "tasks", "grades"];

pub fn make(kind: &str) -> Result<Vec<u8>, String> {
    let mut wb = Workbook::new();
    let e = |e: rust_xlsxwriter::XlsxError| e.to_string();
    match kind {
        "budget" => {
            let ws = wb.add_worksheet();
            ws.set_name("Budget").map_err(e)?;
            for (c, h) in ["Item", "Q1", "Q2", "Q3", "Q4", "Total"].iter().enumerate() {
                ws.write_string(0, c as u16, *h).map_err(e)?;
            }
            for (r, name) in [
                "Rent",
                "Salaries",
                "Software",
                "Travel",
                "Marketing",
                "Supplies",
            ]
            .iter()
            .enumerate()
            {
                let row = r as u32 + 1;
                ws.write_string(row, 0, *name).map_err(e)?;
                for q in 0..4u16 {
                    ws.write_number(row, q + 1, 1000.0 + (r as f64 * 250.0) + (q as f64 * 75.0))
                        .map_err(e)?;
                }
                ws.write_formula(row, 5, format!("=SUM(B{0}:E{0})", row + 1).as_str())
                    .map_err(e)?;
            }
            ws.write_string(8, 0, "Total").map_err(e)?;
            for c in 1..6u16 {
                let col = (b'A' + c as u8) as char;
                ws.write_formula(8, c, format!("=SUM({col}2:{col}7)").as_str())
                    .map_err(e)?;
            }
            let ws = wb.add_worksheet();
            ws.set_name("Notes").map_err(e)?;
            ws.write_string(0, 0, "Assumptions").map_err(e)?;
            ws.write_string(1, 0, "Figures in dollars, per quarter.")
                .map_err(e)?;
        }
        "tasks" => {
            let ws = wb.add_worksheet();
            ws.set_name("Tasks").map_err(e)?;
            for (c, h) in ["Task", "Owner", "Status", "Due"].iter().enumerate() {
                ws.write_string(0, c as u16, *h).map_err(e)?;
            }
            let owners = ["test-a", "test-b", "test-c", "test-d", "agent-claude"];
            let status = ["todo", "doing", "done"];
            for i in 0..12u32 {
                ws.write_string(i + 1, 0, format!("Task {}", i + 1).as_str())
                    .map_err(e)?;
                ws.write_string(i + 1, 1, owners[i as usize % owners.len()])
                    .map_err(e)?;
                ws.write_string(i + 1, 2, status[i as usize % status.len()])
                    .map_err(e)?;
                ws.write_number(i + 1, 3, 46_000.0 + i as f64 * 3.0)
                    .map_err(e)?;
            }
        }
        "grades" => {
            let ws = wb.add_worksheet();
            ws.set_name("Grades").map_err(e)?;
            for (c, h) in ["Student", "Math", "Physics", "History", "Average"]
                .iter()
                .enumerate()
            {
                ws.write_string(0, c as u16, *h).map_err(e)?;
            }
            for i in 0..10u32 {
                ws.write_string(i + 1, 0, format!("Student {}", i + 1).as_str())
                    .map_err(e)?;
                for s in 0..3u16 {
                    ws.write_number(i + 1, s + 1, 55.0 + ((i * 7 + s as u32 * 13) % 45) as f64)
                        .map_err(e)?;
                }
                ws.write_formula(i + 1, 4, format!("=AVERAGE(B{0}:D{0})", i + 2).as_str())
                    .map_err(e)?;
            }
        }
        other => {
            return Err(format!(
                "Unknown sample '{other}' (try {}).",
                SAMPLES.join(", ")
            ));
        }
    }
    wb.save_to_buffer().map_err(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use freedom_sheets::io::load_xlsx_bytes;

    #[test]
    fn samples_read_back() {
        for k in SAMPLES {
            let b = load_xlsx_bytes(&make(k).unwrap()).unwrap();
            let n: usize = b.sheets.iter().map(|s| s.rows.values().map(|r| r.len()).sum::<usize>()).sum();
            assert!(n > 10, "{k}");
        }
        let mut b = load_xlsx_bytes(&make("budget").unwrap()).unwrap();
        assert_eq!(b.sheets.len(), 2);
        let f = b.sheets[0].get(1, 5).unwrap();
        assert_eq!(f.formula.as_ref().unwrap().text, "SUM(B2:E2)");
        b.recalc();
        assert!(b.sheets[0].value(1, 5).as_number().is_some());
    }
}
