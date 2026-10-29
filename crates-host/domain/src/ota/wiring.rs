//! Вход узла `OTA` (`OtaWiring`) и режим применения образа (`Mode`).

use defmt_or_log::{FormatOrDebug, warn};
use ports::{FirmwareUpdate, Rejection, SIGNATURE_BYTES};

use crate::ota::rejection::update_rejection;
use crate::ota::update;

/// Подпись образа.
pub(super) type Signature = [u8; SIGNATURE_BYTES];

/// Вход узла OTA: адаптер обновления `F`, чем перезагружать МК и как
/// применять образ. Адаптер над разделами платы строит compose-site:
///
/// ```ignore
// {%- if graph == "true" %}
/// fragments: [::domain::OTA_FRAG<::adapters::ota::Updater<bsp::FirmwareFlash, false>,
///                                bsp::reset::Reboot> as OTA
///                 = ::domain::ota::OtaWiring { flash: ::adapters::ota::Updater::new(board.firmware),
///                                              reboot: bsp::reset::Reboot,
///                                              mode: ::domain::ota::Mode::Plain }];
// {%- else %}
/// // crates-cross/app/src/tasks/ota.rs
/// let inputs = ::domain::ota::OtaWiring { flash: adapters::ota::Updater::new(board.firmware),
///                                         reboot: bsp::reset::Reboot,
///                                         mode: ::domain::ota::Mode::Plain };
// {%- endif %}
/// ```
///
/// Тип объявлен здесь, а не назван compose-site, по правилу фрагментов
/// `supervisor`: подсистема говорит, что ей нужно, приложение это раздаёт —
/// зависимость направлена от приложения к подсистеме и никогда обратно.
#[derive(Debug)]
pub struct OtaWiring<F, R> {
    /// Адаптер обновления над разделами платы.
    pub flash: F,
    /// Перезагрузка МК — для команды `Reboot`.
    pub reboot: R,
    /// Режим применения: `Mode::Plain` или `Mode::Signed`.
    pub mode: Mode,
}

/// Чем режимы отличаются: проверка `Start` до стирания и применение.
///
/// Данные, а не трейт и не указатели на функции: применение асинхронное,
/// а указатель на `async fn` не сохранить; и
/// обоим режимам хватает одного порта [`FirmwareUpdate`] — запись о подписи
/// для bootloader'а это обычная запись в раздел.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Без подписи: принятый образ применяется
    /// [`FirmwareUpdate::mark_updated`] — после сверки раскладки в хвосте
    /// (`Truncated`, `Layout`).
    Plain,
    /// С подписью: принятый образ применяется
    /// [`apply_signed`](crate::ota::update::apply_signed) — длина, занятость,
    /// раскладка, версия, в этом порядке, и запись подписи для bootloader'а, который её и
    /// проверит перед обменом.
    ///
    /// Отличие до приёма одно: `Start` без подписи отвергается ДО стирания
    /// раздела — применить такой образ всё равно нечем, а стирание уничтожило
    /// бы образ, в который устройство откатывается.
    Signed {
        /// Версия этой прошивки (`FW_VERSION` приложения) — защита от отката.
        running_version: u32,
    },
}

impl Mode {
    /// Проверка `Start` ДО стирания раздела.
    pub(super) fn check(self, signature: Option<&Signature>) -> Result<(), Rejection> {
        match self {
            Self::Plain => Ok(()),
            Self::Signed { .. } if signature.is_some() => Ok(()),
            Self::Signed { .. } => Err(Rejection::Signature),
        }
    }

    /// Применение принятого образа длиной `len`.
    pub(super) async fn apply<F>(
        self,
        flash: &mut F,
        signature: Option<&Signature>,
        len: u32,
    ) -> Result<(), Rejection>
    where
        F: FirmwareUpdate,
        F::Error: FormatOrDebug,
    {
        match self {
            // Без подписи применение — просьба об обмене разделов; версию
            // она не проверяет намеренно (см. `FirmwareUpdate::mark_updated`),
            // а раскладку — да: это защита от ошибки сборки, а не от подделки.
            Self::Plain => {
                update::check_layout(flash, len).await.map_err(|err| {
                    warn!("ota: образ отвергнут: {:?}", &err);
                    update_rejection(&err)
                })?;
                flash.mark_updated().await.map_err(|err| {
                    warn!("ota: пометить обмен не удалось: {:?}", &err);
                    Rejection::Device
                })
            }
            Self::Signed { running_version } => {
                // `check` уже отказал бы без подписи; `ok_or` — чтобы не
                // паниковать, а не потому что отказ здесь достижим.
                let signature = signature.ok_or(Rejection::Signature)?;
                update::apply_signed(flash, running_version, signature, len)
                    .await
                    .map_err(|err| {
                        warn!("ota: применение отвергнуто: {:?}", &err);
                        update_rejection(&err)
                    })
            }
        }
    }
}
