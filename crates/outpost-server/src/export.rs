//! Ф6: срез данных заказчика в single-tenant seed для on-прем сборки.
//!
//! На вход — исходная БД хаба и `customer_id`; на выходе — свежая БД, куда
//! применены те же миграции (схема + глобальные lookup-сиды: роли, права,
//! дефолтные настройки) и в которую скопированы данные ТОЛЬКО указанного
//! арендатора.
//!
//! Правила копирования (перечень таблиц берётся из схемы динамически):
//!  - `customers` — очистить сид по умолчанию, вставить строку целевого
//!    заказчика (идентификатор — `id`, не `customer_id`);
//!  - таблицы со столбцом `customer_id` — очистить и вставить строки
//!    `WHERE customer_id = ?`;
//!  - глобальные lookup-таблицы без `customer_id` (роли/права/гранты и т.п.) —
//!    оставить как засеяли миграции (не трогать).
//!
//! Внешние ключи на время копирования выключены (`PRAGMA foreign_keys=OFF`),
//! поэтому порядок вставок не важен. Идентификаторы сохраняются как есть —
//! перенумерации нет; SQLite сам обновляет `sqlite_sequence` при вставке строк
//! с явными rowid. Исходная и целевая БД обязаны быть на одной версии схемы
//! (пайплайн собирает бинарь и делает срез из текущего хаба).

use sqlx::{Connection, SqliteConnection};

#[derive(Debug, Default)]
pub struct ExportReport {
    /// Скопировано строк по таблицам (только customer-scoped + customers).
    pub per_table: Vec<(String, u64)>,
    /// Итог строк.
    pub total_rows: u64,
    /// Глобальные таблицы, оставленные как засеяно.
    pub global_tables: Vec<String>,
}

/// Выполнить срез: применить миграции к `target`, скопировать данные заказчика
/// `customer_id` из `source`. `target` не должен существовать.
pub async fn export_customer(
    source: &str,
    target: &str,
    customer_id: i64,
) -> anyhow::Result<ExportReport> {
    if std::path::Path::new(target).exists() {
        anyhow::bail!("целевой файл {target} уже существует — удалите его");
    }
    if !std::path::Path::new(source).exists() {
        anyhow::bail!("исходная БД {source} не найдена");
    }

    // 1. Схема + глобальные сиды в целевой БД.
    {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{target}?mode=rwc"))
            .await?;
        outpost_migrations::run(&pool).await?;
        pool.close().await;
    }

    // 2. Срез через одно соединение с ATTACH исходной БД.
    let mut conn = SqliteConnection::connect(&format!("sqlite://{target}")).await?;
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut conn)
        .await?;
    let attach = format!("ATTACH DATABASE '{}' AS src", source.replace('\'', "''"));
    sqlx::query(&attach).execute(&mut conn).await?;

    // Сверка версий схемы: количество применённых миграций должно совпасть.
    let tgt_mig: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM main._sqlx_migrations")
        .fetch_one(&mut conn)
        .await?;
    let src_mig: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM src._sqlx_migrations")
        .fetch_one(&mut conn)
        .await?;
    if tgt_mig != src_mig {
        anyhow::bail!(
            "версии схемы расходятся: цель={tgt_mig} миграций, источник={src_mig}; \
             соберите срез из хаба той же версии"
        );
    }

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' \
         AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations' ORDER BY name",
    )
    .fetch_all(&mut conn)
    .await?;

    let mut report = ExportReport::default();
    for t in &tables {
        let cols: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{t}')"))
                .fetch_all(&mut conn)
                .await?;
        let has_customer = cols.iter().any(|c| c == "customer_id");

        if t == "customers" {
            sqlx::query("DELETE FROM main.customers")
                .execute(&mut conn)
                .await?;
            let n = sqlx::query("INSERT INTO main.customers SELECT * FROM src.customers WHERE id = ?")
                .bind(customer_id)
                .execute(&mut conn)
                .await?
                .rows_affected();
            report.per_table.push((t.clone(), n));
            report.total_rows += n;
        } else if has_customer {
            sqlx::query(&format!("DELETE FROM main.{t}"))
                .execute(&mut conn)
                .await?;
            let n = sqlx::query(&format!(
                "INSERT INTO main.{t} SELECT * FROM src.{t} WHERE customer_id = ?"
            ))
            .bind(customer_id)
            .execute(&mut conn)
            .await?
            .rows_affected();
            report.per_table.push((t.clone(), n));
            report.total_rows += n;
        } else {
            report.global_tables.push(t.clone());
        }
    }

    sqlx::query("DETACH DATABASE src").execute(&mut conn).await?;
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut conn)
        .await?;
    conn.close().await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn seed_source(path: &str) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{path}?mode=rwc"))
            .await
            .unwrap();
        outpost_migrations::run(&pool).await.unwrap();
        // Второй заказчик + устройства у обоих.
        sqlx::query("INSERT INTO customers (id, name) VALUES (2, 'второй')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO devices (customer_id, serial, display_name, device_class) \
             VALUES (1, 'A-1', 'своё', 'android_tactical'), \
                    (2, 'B-1', 'чужое', 'wearable')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn slices_only_target_customer() {
        let dir = std::env::temp_dir();
        let uniq = format!("{}", std::process::id());
        let source = dir.join(format!("exp_src_{uniq}.db"));
        let target = dir.join(format!("exp_tgt_{uniq}.db"));
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&target);
        let src = source.to_str().unwrap();
        let tgt = target.to_str().unwrap();

        seed_source(src).await;
        let report = export_customer(src, tgt, 1).await.unwrap();
        assert!(report.total_rows >= 1, "должна скопироваться хотя бы 1 строка");

        // Проверить целевую БД: только заказчик 1, только его устройство,
        // глобальные роли на месте.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{tgt}"))
            .await
            .unwrap();
        let customers: Vec<i64> = sqlx::query_scalar("SELECT id FROM customers")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(customers, vec![1], "в срезе должен остаться только заказчик 1");

        let foreign: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE customer_id != 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(foreign, 0, "чужих устройств быть не должно");

        let own: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE customer_id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(own, 1, "своё устройство должно сохраниться");

        let roles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_roles")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(roles > 0, "глобальные роли должны быть засеяны миграциями");

        pool.close().await;
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&target);
    }
}
