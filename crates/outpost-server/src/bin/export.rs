//! `outpost-export` — CLI-обёртка среза заказчика (Ф6).
//!
//! Использование:
//!   outpost-export <источник.db> <цель.db> <customer_id>
//!
//! Создаёт `цель.db` (свежая, с применёнными миграциями) и копирует туда
//! данные только указанного заказчика. См. `outpost_server::export`.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: outpost-export <source.db> <target.db> <customer_id>");
        std::process::exit(2);
    }
    let source = &args[1];
    let target = &args[2];
    let customer_id: i64 = args[3]
        .parse()
        .map_err(|_| anyhow::anyhow!("customer_id должен быть числом"))?;

    let report = outpost_server::export::export_customer(source, target, customer_id).await?;

    for (t, n) in &report.per_table {
        println!("  {t}: {n} строк");
    }
    println!(
        "глобальных таблиц оставлено как засеяно: {}",
        report.global_tables.len()
    );
    println!(
        "срез готов: заказчик {customer_id} → {target} ({} строк скопировано)",
        report.total_rows
    );
    Ok(())
}
