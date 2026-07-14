//! Назначение тенанта и видимость пунктов верхнего меню.
//!
//! У каждого тенанта есть `customers.purpose` (миграция 0037) — то, ДЛЯ ЧЕГО
//! тенант предназначен: тактическое подразделение, антидронный объект, игра
//! класса STALKER, парк носимых устройств или исторический «всё сразу».
//! Верхнее меню (`templates/_nav.html`) рендерится из [`NavCtx`], поэтому
//! игровой тенант видит только игровые разделы, антидронный — антидронные.
//!
//! Скрытие пункта меню — НЕ контроль доступа: страницы остаются доступными по
//! прямому URL, данные в них изолированы по `customer_id` как и всюду.

/// Назначение тенанта. Хранится в `customers.purpose` строкой ([`Self::as_str`]);
/// whitelist здесь, а не в CHECK-констрейнте — новое назначение добавляется
/// без миграции (тот же подход, что у `customers.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantPurpose {
    /// Показывать всё (поведение до миграции 0037; default для старых тенантов).
    Universal,
    Tactical,
    Antidrone,
    Game,
    Wearables,
}

impl TenantPurpose {
    pub const ALL: &'static [TenantPurpose] = &[
        TenantPurpose::Universal,
        TenantPurpose::Tactical,
        TenantPurpose::Antidrone,
        TenantPurpose::Game,
        TenantPurpose::Wearables,
    ];

    /// Разбор значения из БД или формы. Незнакомая строка — `None`; вызывающий
    /// код падает обратно на `Universal`: испорченное поле в БД не должно
    /// оставить админа без меню.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "universal" => Self::Universal,
            "tactical" => Self::Tactical,
            "antidrone" => Self::Antidrone,
            "game" => Self::Game,
            "wearables" => Self::Wearables,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Universal => "universal",
            Self::Tactical => "tactical",
            Self::Antidrone => "antidrone",
            Self::Game => "game",
            Self::Wearables => "wearables",
        }
    }

    /// Подпись для бейджа и дропдауна «Назначение». Admin UI намеренно
    /// русскоязычный (см. `crate::i18n`), поэтому подписи здесь без locale.
    pub fn label_ru(self) -> &'static str {
        match self {
            Self::Universal => "универсальный",
            Self::Tactical => "тактический",
            Self::Antidrone => "антидрон",
            Self::Game => "игра",
            Self::Wearables => "носимые",
        }
    }
}

/// Всё, что нужно `_nav.html`. По флагу на каждый пункт меню, зависящий от
/// назначения тенанта; всегда видимые пункты (Сводка, Устройства, Группы,
/// Телеметрия, Пользователи, Роли, Настройки) флагов не имеют. «Тенанты» —
/// по `is_super_admin`.
#[derive(Debug, Clone)]
pub struct NavCtx {
    pub login: String,
    pub is_super_admin: bool,
    /// Имя активного тенанта — бейдж в adminbar (важно для super-admin,
    /// переключённого в чужой тенант через `outpost_acting`).
    pub tenant_name: String,
    pub tenant_purpose_label: &'static str,
    pub show_map_tactical: bool,
    pub show_map_antidrone: bool,
    pub show_map_players: bool,
    pub show_map_wearables: bool,
    pub show_apps: bool,
    pub show_configs: bool,
    pub show_files: bool,
    pub show_push: bool,
    pub show_ballistics: bool,
}

/// Карта «назначение → видимые пункты». Приложения/Конфигурации/Файлы/Push
/// показываются только там, где парк — управляемые Android-устройства
/// (tactical, game); антидронные узлы (ESP32) и носимые APK не получают.
pub fn build(
    purpose: TenantPurpose,
    login: &str,
    tenant_name: &str,
    is_super_admin: bool,
) -> NavCtx {
    use TenantPurpose::*;
    NavCtx {
        login: login.to_string(),
        is_super_admin,
        tenant_name: tenant_name.to_string(),
        tenant_purpose_label: purpose.label_ru(),
        show_map_tactical: matches!(purpose, Universal | Tactical),
        show_map_antidrone: matches!(purpose, Universal | Antidrone),
        show_map_players: matches!(purpose, Universal | Game),
        show_map_wearables: matches!(purpose, Universal | Wearables),
        show_apps: matches!(purpose, Universal | Tactical | Game),
        show_configs: matches!(purpose, Universal | Tactical | Game),
        show_files: matches!(purpose, Universal | Tactical | Game),
        show_push: matches!(purpose, Universal | Tactical | Game),
        show_ballistics: matches!(purpose, Universal | Tactical),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrips_every_purpose_and_rejects_garbage() {
        for p in TenantPurpose::ALL {
            assert_eq!(TenantPurpose::parse(p.as_str()), Some(*p));
        }
        assert_eq!(TenantPurpose::parse(""), None);
        assert_eq!(TenantPurpose::parse("production"), None);
        assert_eq!(TenantPurpose::parse("Universal"), None);
    }

    fn flags(p: TenantPurpose) -> [bool; 9] {
        let n = build(p, "admin", "t", false);
        [
            n.show_map_tactical,
            n.show_map_antidrone,
            n.show_map_players,
            n.show_map_wearables,
            n.show_apps,
            n.show_configs,
            n.show_files,
            n.show_push,
            n.show_ballistics,
        ]
    }

    #[test]
    fn universal_shows_everything() {
        assert_eq!(flags(TenantPurpose::Universal), [true; 9]);
    }

    #[test]
    fn tactical_hides_other_maps() {
        assert_eq!(
            flags(TenantPurpose::Tactical),
            [true, false, false, false, true, true, true, true, true]
        );
    }

    #[test]
    fn antidrone_shows_only_its_map() {
        assert_eq!(
            flags(TenantPurpose::Antidrone),
            [false, true, false, false, false, false, false, false, false]
        );
    }

    #[test]
    fn game_shows_players_and_apk_pipeline_without_ballistics() {
        assert_eq!(
            flags(TenantPurpose::Game),
            [false, false, true, false, true, true, true, true, false]
        );
    }

    #[test]
    fn wearables_shows_only_its_map() {
        assert_eq!(
            flags(TenantPurpose::Wearables),
            [false, false, false, true, false, false, false, false, false]
        );
    }

    #[test]
    fn super_admin_flag_and_identity_pass_through() {
        let n = build(TenantPurpose::Game, "game-admin", "Игровой", true);
        assert!(n.is_super_admin);
        assert_eq!(n.login, "game-admin");
        assert_eq!(n.tenant_name, "Игровой");
        assert_eq!(n.tenant_purpose_label, "игра");
    }
}
