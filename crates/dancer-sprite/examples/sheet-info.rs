//! Loads a sheet and reports what the loader made of it.
//!
//! Exists to check FAOSDance compatibility against real sheets without running
//! the whole app:
//!
//!     cargo run -p dancer-sprite --example sheet-info -- path/to/Dance_Large.png

use dancer_sprite::Sheet;

fn main() -> anyhow::Result<()> {
    tracing_init();
    let path = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "assets/default.png".into());

    let sheet = Sheet::load(&path)?;

    println!(
        "\n{}\n  cell {}x{}, {} cells/row, {} rows",
        path.display(),
        sheet.cell_width,
        sheet.cell_height,
        sheet.cells_per_row(),
        sheet.rows.len()
    );
    println!(
        "  default row: {} ({})",
        sheet.default_row, sheet.rows[sheet.default_row].name
    );
    println!(
        "  held row   : {}",
        sheet
            .held_row
            .map(|i| format!("{i} ({})", sheet.rows[i].name))
            .unwrap_or_else(|| "none".into())
    );

    println!("\n  idx  name              opaque%  (per cell)");
    for (i, row) in sheet.rows.iter().enumerate() {
        // Coverage per cell is a cheap sanity check: a correctly sliced sheet
        // shows the figure moving, not a constant or an empty row.
        let cov: Vec<String> = row
            .cells
            .iter()
            .map(|c| {
                let opaque = c.iter().filter(|p| (*p >> 24) > 8).count();
                format!("{:>3}", opaque * 100 / c.len().max(1))
            })
            .collect();
        println!("  {i:>3}  {:<16}  {}", row.name, cov.join(" "));
    }
    println!();
    Ok(())
}

fn tracing_init() {
    // Keep the example quiet unless RUST_LOG asks otherwise.
    let _ = std::env::var("RUST_LOG");
}
