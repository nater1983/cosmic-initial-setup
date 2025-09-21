use cosmic::{
    Element, Task,
    cosmic_config::{self, ConfigSet},
    cosmic_theme,
    iced::Alignment,
    theme, widget,
};
use eyre::Context;
use slotmap::{DefaultKey, SlotMap};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::fs::{File, set_permissions};
use tokio::io::AsyncWriteExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::{fl, page};

#[derive(Clone, Debug)]
pub enum Message {
    Refresh(Arc<eyre::Result<PageRefresh>>),
    Search(String),
    Select(DefaultKey),
}

impl From<Message> for super::Message {
    fn from(message: Message) -> Self {
        super::Message::Language(message).into()
    }
}

#[derive(Debug)]
pub struct PageRefresh {
    config: Option<cosmic_config::Config>,
    registry: Registry,
    language: Option<SystemLocale>,
    region: Option<SystemLocale>,
    available_languages: SlotMap<DefaultKey, SystemLocale>,
    system_locales: BTreeMap<String, SystemLocale>,
    selected: DefaultKey,
}

pub struct Page {
    selected: DefaultKey,
    search_id: widget::Id,
    search: String,
    regex_opt: Option<regex::Regex>,
    active_context: Vec<DefaultKey>,
    config: Option<cosmic_config::Config>,
    registry: Option<locales_rs::Registry>,
    language: Option<SystemLocale>,
    region: Option<SystemLocale>,
    available_languages: SlotMap<DefaultKey, SystemLocale>,
    system_locales: BTreeMap<String, SystemLocale>,
}

impl Page {
    pub fn new() -> Self {
        Self {
            selected: DefaultKey::null(),
            search_id: widget::Id::unique(),
            search: String::new(),
            regex_opt: None,
            active_context: Vec::new(),
            config: None,
            registry: None,
            language: None,
            region: None,
            available_languages: SlotMap::default(),
            system_locales: BTreeMap::new(),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<page::Message> {
        match message {
            Message::Search(search) => {
                self.search = search;
                self.regex_opt = None;
                if !self.search.is_empty() {
                    let pattern = regex::escape(&self.search);
                    match regex::RegexBuilder::new(&pattern)
                        .case_insensitive(true)
                        .build()
                    {
                        Ok(regex) => self.regex_opt = Some(regex),
                        Err(err) => {
                            tracing::warn!(
                                err = err.to_string(),
                                "failed to parse regex {:?}",
                                pattern
                            );
                        }
                    };
                }

                self.update_context();
            }

            Message::Select(selected) => {
                if let Some(locale) = self.available_languages.get(selected) {
                    let lang = locale.lang_code.clone();
                    let region = lang.clone();
                    tokio::spawn(crate::locales::set_locale(lang, region));

                    if let Some(config) = self.config.as_mut() {
                        _ = config.set("system_locales", vec![locale.lang_code.clone()]);
                    }

                    crate::localize::set_locale(&locale.lang_code);
                }

                self.selected = selected;
            }

            Message::Refresh(result) => {
                match Arc::into_inner(result).unwrap() {
                    Ok(page_refresh) => {
                        self.config = page_refresh.config;
                        self.available_languages = page_refresh.available_languages;
                        self.system_locales = page_refresh.system_locales;
                        self.language = page_refresh.language;
                        self.region = page_refresh.region;
                        self.registry = Some(page_refresh.registry.0);
                        self.selected = page_refresh.selected;
                    }

                    Err(why) => {
                        tracing::error!(?why, "failed to get locales from the system");
                    }
                }

                self.update_context();
            }
        }
        Task::none()
    }

    fn update_context(&mut self) {
        self.active_context = self
            .available_languages
            .iter()
            .filter_map(|(id, locale)| {
                let Some(regex) = &self.regex_opt else {
                    return Some(id);
                };

                if regex.is_match(&locale.display_name) {
                    return Some(id);
                }

                None
            })
            .take(100)
            .collect::<Vec<DefaultKey>>();
    }
}

impl super::Page for Page {
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn title(&self) -> String {
        fl!("select-language-page")
    }

    fn init(&mut self) -> cosmic::Task<page::Message> {
        let refresh = async || -> eyre::Result<PageRefresh> {
            let registry = locales_rs::Registry::new().wrap_err("failed to get locale registry")?;

            // Collect system locales
            let mut available_languages_set = BTreeSet::new();
            let output = tokio::process::Command::new("locale")
                .arg("-a")
                .output()
                .await
                .wrap_err("failed to run locale -a")?;
            let output = String::from_utf8(output.stdout).unwrap_or_default();

            for line in output.lines() {
                if line == "C" || line == "POSIX" {
                    continue;
                }
                if let Some(locale) = registry.locale(line) {
                    available_languages_set.insert(localized_locale(&locale, line.to_owned()));
                }
            }

            let mut available_languages = SlotMap::new();
            let mut selected = DefaultKey::null();

            let current_lang = std::env::var("LANG").ok();
            if let Some(lang) = current_lang.as_ref() {
                if let Some(locale) = registry.locale(lang) {
                    selected = available_languages.insert(localized_locale(&locale, lang.clone()));
                }
            }

            for language in available_languages_set {
                available_languages.insert(language);
            }

            Ok(PageRefresh {
                config: cosmic::cosmic_config::Config::new("com.system76.CosmicSettings", 1).ok(),
                registry: Registry(registry),
                language: current_lang.clone().and_then(|l| registry.locale(&l)).map(|l| localized_locale(&l, l.lang_code.clone())),
                region: current_lang.clone().and_then(|l| registry.locale(&l)).map(|l| localized_locale(&l, l.lang_code.clone())),
                available_languages,
                system_locales: BTreeMap::new(),
                selected,
            })
        };

        cosmic::task::future(async move { Message::Refresh(Arc::new(refresh().await)) })
    }

    fn open(&mut self) -> cosmic::Task<page::Message> {
        widget::text_input::focus(self.search_id.clone())
    }

    fn completed(&self) -> bool {
        !self.selected.is_null()
    }

    fn view(&self) -> Element<'_, page::Message> {
        let cosmic_theme::Spacing { space_xxs, space_m, .. } = theme::active().cosmic().spacing;

        let mut section = widget::settings::section();
        let mut first_opt = None;

        for (id, locale) in self.active_context.iter().filter_map(|id| self.available_languages.get(*id).map(|l| (*id, l))) {
            let item = widget::settings::item::builder(&locale.display_name);
            let selected = id == self.selected;

            section = section.add(
                widget::button::custom(
                    item.control(
                        widget::row::with_children(vec![
                            if selected {
                                widget::icon::from_name("object-select-symbolic").size(16).into()
                            } else {
                                widget::Space::with_width(16).into()
                            }
                        ])
                        .align_y(Alignment::Center)
                        .spacing(space_xxs),
                    ),
                )
                .on_press(Message::Select(id.clone()))
                .class(if selected { theme::Button::Link } else { theme::Button::MenuRoot }),
            );

            if first_opt.is_none() {
                first_opt = Some(id);
            }
        }

        let mut search_input = widget::search_input(fl!("type-to-search"), &self.search)
            .id(self.search_id.clone())
            .on_input(Message::Search);

        if let Some(first) = first_opt {
            if self.regex_opt.is_some() {
                search_input = search_input.on_submit(move |_| Message::Select(first.clone()));
            }
        }

        widget::column::with_children(vec![
            search_input.into(),
            widget::Space::with_height(space_m).into(),
            widget::scrollable(section).into(),
        ]).into()
         .map(page::Message::Language)
    }
}

// Slackware-compatible set_locale function
pub async fn set_locale(lang: String, region: String) -> std::io::Result<()> {
    let path = Path::new("/etc/profile.d/lang.sh");
    let mut file = File::create(path).await?;

    let content = format!(
        "export LANG={lang}\n\
         export LC_ADDRESS={region}\n\
         export LC_IDENTIFICATION={region}\n\
         export LC_MEASUREMENT={region}\n\
         export LC_MONETARY={region}\n\
         export LC_NAME={region}\n\
         export LC_NUMERIC={region}\n\
         export LC_PAPER={region}\n\
         export LC_TELEPHONE={region}\n\
         export LC_TIME={region}\n",
        lang = lang,
        region = region
    );

    file.write_all(content.as_bytes()).await?;

    // Set permissions to 0755
    set_permissions(path, std::fs::Permissions::from_mode(0o755)).await?;

    Ok(())
}

// --- locale helpers ---
fn localized_iso_codes(locale: &locales_rs::Locale) -> (String, String) {
    let mut language = gettextrs::dgettext("iso_639", &locale.language.display_name);
    let country = gettextrs::dgettext("iso_3166", &locale.territory.display_name);

    let mut chars = language.chars();
    if let Some(c) = chars.next() {
        language = c.to_uppercase().collect::<String>() + chars.as_str();
    }

    (language, country)
}

fn localized_locale(locale: &locales_rs::Locale, lang_code: String) -> SystemLocale {
    let (language, country) = localized_iso_codes(locale);
    SystemLocale {
        lang_code,
        display_name: format!("{language} ({country})"),
    }
}

struct Registry(locales_rs::Registry);

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish()
    }
}

#[derive(Clone, Debug)]
pub struct SystemLocale {
    lang_code: String,
    display_name: String,
}

impl Eq for SystemLocale {}
impl Ord for SystemLocale {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.display_name.cmp(&other.display_name)
    }
}
impl PartialOrd for SystemLocale {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.display_name.partial_cmp(&other.display_name)
    }
}
impl PartialEq for SystemLocale {
    fn eq(&self, other: &Self) -> bool {
        self.display_name == other.display_name
    }
}
