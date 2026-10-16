//! Решение bootloader'а при старте: обменивать ли разделы и — под фичей
//! `signed` — пускать ли новый образ, проверив его подпись. Над
//! **блокирующими** разделами: у bootloader'а нет исполнителя.

#[cfg(feature = "signed")]
use embassy_boot::AlignedBuffer;
use embassy_boot::{BlockingFirmwareState, State};
#[cfg(feature = "signed")]
use ports::PendingImage;

#[cfg(feature = "signed")]
use crate::ota::Error;
use crate::ota::MAX_WRITE_SIZE;

/// Что bootloader'у делать при старте — см. [`boot_plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootPlan {
    /// `STATE` не читается: загрузить текущий образ, разделы не трогать.
    BootCurrent,
    /// Обмен запрошен или прерван посередине: строгий
    /// `BootLoader::prepare` — его ошибка обязана кончиться паникой и
    /// сбросом, а не прыжком в `ACTIVE`.
    Swap,
    /// Обмена нет: `BootLoader::try_prepare` только читает, и его ошибку
    /// можно пережить, загрузив текущий образ.
    Boot,
}

/// Решение bootloader'а с разделами на внешней флеши (`ota=external`):
/// обменивать или грузить текущий образ.
///
/// Внешняя микросхема — отдельная деталь платы (не распаяна, не подставлен
/// драйвер, отказала), и из-за неё устройство не должно перестать
/// загружаться: образ в `ACTIVE` внутренний и, пока обмен его не тронул, цел.
/// Но прыжок в `ACTIVE` посреди обмена — смесь старых и новых страниц:
/// `HardFault`, а дойди такой образ до подтверждения — битый образ навсегда.
/// Отсюда три исхода по `STATE`:
///
/// - не читается — [`BootPlan::BootCurrent`]: был ли начат обмен, узнать
///   нечем; не начатый — частый случай (заглушка, не распаянная микросхема), а
///   отказ флеши посреди обмена, не проходящий и после сброса, — редкий;
/// - `Swap` — [`BootPlan::Swap`]: магия держится всё время обмена (и отката),
///   так что прерванный обмен узнаётся по ней же;
/// - иначе — [`BootPlan::Boot`].
///
/// # Panics
///
/// Если `STATE::WRITE_SIZE` шире [`MAX_WRITE_SIZE`] — на STM32 и
/// распространённых SPI NOR такого нет.
pub fn boot_plan<STATE: embedded_storage::nor_flash::NorFlash>(state: &mut STATE) -> BootPlan {
    let mut word = [0; MAX_WRITE_SIZE];
    match BlockingFirmwareState::new(state, &mut word[..STATE::WRITE_SIZE]).get_state() {
        Ok(State::Swap) => BootPlan::Swap,
        Ok(_) => BootPlan::Boot,
        Err(_) => BootPlan::BootCurrent,
    }
}

/// Что решил bootloader об обновлении — см. [`screen_update`].
#[cfg(feature = "signed")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screening {
    /// Нового запроса на обмен нет: обычный старт, обмен, прерванный на
    /// середине, или откат неподтверждённого образа. Проверять нечего —
    /// `embassy-boot` делает своё.
    NothingToCheck,
    /// Образ в `DFU` подписан верно — обмен пусть идёт.
    Approved,
    /// Образ отвергнут, запрос на обмен снят (`STATE` → `Boot`): загрузится
    /// прежний образ.
    Rejected(Rejection),
}

/// Почему образ в `DFU` отвергнут.
#[cfg(feature = "signed")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Обмен запрошен, а записи о принятом образе нет — образ положили мимо
    /// `domain::ota::update::apply_signed`, то есть без подписи.
    NoRecord,
    /// Длина в записи — ноль или больше `ACTIVE`.
    Length,
    /// Открытый ключ — нули, то есть не подставлен. Отдельной проверкой, а не
    /// «с нулями всё равно не сойдётся»: нулевой ключ ed25519 — точка малого
    /// порядка, подпись под него подделывается, и `salty` такие не отсеивает.
    NoPublicKey,
    /// Ключ не разбирается как точка кривой или подпись не сошлась.
    Signature,
}

/// Проверка нового образа bootloader'ом — один раз, перед обменом.
///
/// Проверяет только **новый** запрос: магия `Swap` в `STATE` и пустой журнал
/// прогресса. Та же магия держится и после обмена, пока новый образ не
/// подтвердится, — тогда `embassy-boot` откатывает его, и запись к этому
/// времени уже затёрта обменом; проверять там нечего, а «отвергнуть» значило бы
/// сорвать откат. Журнал читается без знания стёртого значения (у части чипов
/// это `0x00`): при магии `Swap` слово 1 («журнал действителен») всегда
/// стёрто, поэтому «обмен не начинался» — это слово 2 (первая отметка
/// прогресса), побайтно равное слову 1. Формат журнала — внутренний у
/// `embassy-boot` (doc у `BootLoader::state`), и привязку к нему держит
/// host-тест с настоящим обменом `embassy-boot`.
///
/// Отказ снимает запрос: в `STATE` пишется магия `Boot` (`mark_booted`), и
/// следующий за этим `BootLoader::prepare` грузит прежний образ.
///
/// Время: SHA-512 всего образа и одна проверка ed25519 — порядка сотни
/// миллионов тактов в портируемой реализации. Сторож к этому моменту не
/// запущен: проверка — один неделимый вызов, покормить его внутри нечем.
///
/// # Errors
///
/// Ошибка флеша при чтении разделов или при снятии запроса.
///
/// # Panics
///
/// Если `STATE::WRITE_SIZE` шире [`MAX_WRITE_SIZE`] или не помещается в
/// `u32` — на STM32 слово записи не больше 32 байт, так что на практике нет.
#[cfg(feature = "signed")]
pub fn screen_update<DFU, STATE>(
    dfu: &mut DFU,
    state: &mut STATE,
    capacity: u32,
    public_key: &[u8; 32],
) -> Result<Screening, Error>
where
    DFU: embedded_storage::nor_flash::NorFlash,
    STATE: embedded_storage::nor_flash::NorFlash,
{
    let mut aligned = AlignedBuffer([0; MAX_WRITE_SIZE]);
    let word = &mut aligned.0[..STATE::WRITE_SIZE];
    if BlockingFirmwareState::new(&mut *state, &mut *word).get_state()? != State::Swap {
        return Ok(Screening::NothingToCheck);
    }
    let mut validity = [0; MAX_WRITE_SIZE];
    let mut first_step = [0; MAX_WRITE_SIZE];
    let size = u32::try_from(STATE::WRITE_SIZE).expect("WRITE_SIZE не должен превышать u32::MAX");
    state.read(size, &mut validity[..STATE::WRITE_SIZE])?;
    state.read(2 * size, &mut first_step[..STATE::WRITE_SIZE])?;
    if validity != first_step {
        return Ok(Screening::NothingToCheck);
    }

    let verdict = check_pending(dfu, capacity, public_key)?;
    if let Err(rejection) = verdict {
        BlockingFirmwareState::new(&mut *state, &mut aligned.0[..STATE::WRITE_SIZE])
            .mark_booted()?;
        return Ok(Screening::Rejected(rejection));
    }
    Ok(Screening::Approved)
}

/// Сама проверка записи и подписи — без решения, что делать с отказом.
#[cfg(feature = "signed")]
fn check_pending<DFU: embedded_storage::nor_flash::NorFlash>(
    dfu: &mut DFU,
    capacity: u32,
    public_key: &[u8; 32],
) -> Result<Result<(), Rejection>, Error> {
    /// Сколько байт образа читается за раз на хеш.
    const CHUNK: u32 = 256;
    let mut record = [0; PendingImage::LEN];
    dfu.read(capacity, &mut record)?;
    let Some(pending) = PendingImage::from_bytes(&record) else {
        return Ok(Err(Rejection::NoRecord));
    };
    if pending.len == 0 || pending.len > capacity {
        return Ok(Err(Rejection::Length));
    }
    if public_key.iter().all(|byte| *byte == 0) {
        return Ok(Err(Rejection::NoPublicKey));
    }

    let mut digest = salty::Sha512::new();
    let mut chunk = [0; CHUNK as usize];
    let mut offset = 0;
    while offset < pending.len {
        let len = CHUNK.min(pending.len - offset);
        let bytes = &mut chunk[..len as usize];
        dfu.read(offset, bytes)?;
        digest.update(bytes);
        offset += len;
    }
    let message = digest.finalize();

    let Ok(key) = salty::PublicKey::try_from(public_key) else {
        return Ok(Err(Rejection::Signature));
    };
    let signature = salty::Signature::from(&pending.signature);
    if key.verify(&message, &signature).is_ok() {
        Ok(Ok(()))
    } else {
        Ok(Err(Rejection::Signature))
    }
}

#[cfg(test)]
mod tests {
    /// Решение bootloader'а с внешней флешью (`boot_plan`) — над настоящим
    /// обменом `embassy-boot`.
    mod plan {
        use embassy_boot::{BlockingFirmwareState, BootLoader, BootLoaderConfig};
        use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};

        use crate::flash::Unconnected;
        use crate::ota::fakes::{Dfu, MemFlash, State};
        use crate::ota::{BootPlan, boot_plan};

        /// `DFU`, который после `ops` удачных стираний и записей отказывает —
        /// внешняя флеш, отвалившаяся посреди обмена.
        struct Flaky {
            flash: Dfu,
            ops: usize,
        }

        impl Flaky {
            fn tick(&mut self) -> Result<(), NorFlashErrorKind> {
                self.ops = self.ops.checked_sub(1).ok_or(NorFlashErrorKind::Other)?;
                Ok(())
            }
        }

        impl ErrorType for Flaky {
            type Error = NorFlashErrorKind;
        }

        impl ReadNorFlash for Flaky {
            const READ_SIZE: usize = 1;

            fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
                self.flash.read(offset, bytes)
            }

            fn capacity(&self) -> usize {
                self.flash.capacity()
            }
        }

        impl NorFlash for Flaky {
            const WRITE_SIZE: usize = 8;
            const ERASE_SIZE: usize = 256;

            fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
                self.tick()?;
                self.flash.erase(from, to)
            }

            fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
                self.tick()?;
                self.flash.write(offset, bytes)
            }
        }

        fn requested() -> State {
            let mut state = State::erased();
            BlockingFirmwareState::new(&mut state, &mut [0; 8])
                .mark_updated()
                .expect("запросить обмен");
            state
        }

        #[test]
        fn without_a_request_the_bootloader_may_survive_a_flash_error() {
            assert_eq!(boot_plan(&mut State::erased()), BootPlan::Boot);
        }

        #[test]
        fn an_unreadable_state_boots_the_current_image() {
            assert_eq!(boot_plan(&mut Unconnected), BootPlan::BootCurrent);
        }

        #[test]
        fn a_requested_swap_is_strict() {
            assert_eq!(boot_plan(&mut requested()), BootPlan::Swap);
        }

        /// Главное свойство: обмен, прерванный отказом внешней флеши, —
        /// по-прежнему `Swap`, то есть строгий `prepare` (сброс и повтор), а
        /// не прыжок в наполовину переписанный `ACTIVE`. И повтор с ожившей
        /// флешью доводит обмен до конца.
        #[test]
        fn an_interrupted_swap_stays_strict_and_resumes() {
            let image: Vec<u8> = (0..768u32).map(|i| (i % 253) as u8).collect();
            let mut active = MemFlash::<768>::filled(0x11);
            let mut dfu = Flaky {
                flash: Dfu::erased(),
                ops: 0,
            };
            dfu.flash.mem[..768].copy_from_slice(&image);
            let mut state = requested();
            // Пара удачных шагов — и флеш отказывает посреди обмена.
            dfu.ops = 2;

            let interrupted = BootLoader::new(BootLoaderConfig {
                active: &mut active,
                dfu: &mut dfu,
                state: &mut state,
            })
            .prepare_boot(&mut [0; 8]);
            assert!(interrupted.is_err(), "обмен должен был прерваться");

            assert_eq!(boot_plan(&mut state), BootPlan::Swap);

            dfu.ops = usize::MAX;
            BootLoader::new(BootLoaderConfig {
                active: &mut active,
                dfu: &mut dfu,
                state: &mut state,
            })
            .prepare_boot(&mut [0; 8])
            .expect("обмен после сброса");
            assert_eq!(&active.mem[..], &image[..], "обмен доведён до конца");
        }
    }

    /// Проверка нового образа bootloader'ом (`screen_update`): настоящие ключи
    /// `salty` и — для отката — настоящий обмен `embassy-boot` над теми же
    /// фейками, что и выше.
    #[cfg(feature = "signed")]
    mod screening {
        use embassy_boot::{
            BlockingFirmwareState, BootLoader, BootLoaderConfig, State as BootState,
        };

        use ports::PendingImage;

        use crate::ota::fakes::{Dfu, MemFlash, State};
        use crate::ota::{Rejection, Screening, screen_update};

        /// `ACTIVE` — три страницы, как `capacity` у `updater()` выше.
        const CAPACITY: u32 = 768;
        const SEED: [u8; 32] = [42; 32];

        fn public_key() -> [u8; 32] {
            salty::Keypair::from(&SEED).public.to_bytes()
        }

        /// Подпись так, как её ставит `cargo xtask build`: ed25519 над SHA-512
        /// образа.
        fn sign(image: &[u8]) -> [u8; 64] {
            let digest = salty::Sha512::new().updated(image).finalize();
            salty::Keypair::from(&SEED).sign(&digest).to_bytes()
        }

        /// `DFU` с образом и записью о нём и `STATE` с запрошенным обменом —
        /// то, что оставляет приложение перед перезагрузкой.
        fn pending(image: &[u8], record: PendingImage) -> (Dfu, State) {
            let mut dfu = Dfu::erased();
            dfu.mem[..image.len()].copy_from_slice(image);
            let spare = CAPACITY as usize;
            dfu.mem[spare..spare + PendingImage::LEN].copy_from_slice(&record.to_bytes());
            let mut state = State::erased();
            BlockingFirmwareState::new(&mut state, &mut [0; 8])
                .mark_updated()
                .expect("запросить обмен");
            (dfu, state)
        }

        fn state_of(state: &mut State) -> BootState {
            BlockingFirmwareState::new(state, &mut [0; 8])
                .get_state()
                .expect("состояние")
        }

        fn image() -> Vec<u8> {
            (0..600u32).map(|i| (i % 251) as u8).collect()
        }

        /// Сквозной путь через порт: `Updater::signed` готовит раздел, образ
        /// и запись ложатся туда, куда пишет `domain::ota::update::apply_signed`
        /// (смещение `capacity`), — и bootloader находит их там же.
        #[tokio::test]
        async fn what_the_signed_port_writes_is_what_the_bootloader_reads() {
            use ports::FirmwareUpdate;

            let image = image();
            let len = u32::try_from(image.len()).expect("образ короткий");
            let mut u = crate::ota::Updater::signed(Dfu::filled(0x00), State::erased(), CAPACITY);
            u.prepare(len).await.expect("подготовка");
            let mut padded = image.clone();
            padded.resize(image.len().next_multiple_of(8), 0xFF);
            u.write(0, &padded).await.expect("образ");
            let record = PendingImage {
                len,
                signature: sign(&image),
            };
            u.write(CAPACITY, &record.to_bytes()).await.expect("запись");
            u.mark_updated().await.expect("запросить обмен");

            let screening = screen_update(&mut u.dfu, &mut u.state, CAPACITY, &public_key());

            assert_eq!(screening.expect("проверка"), Screening::Approved);
        }

        #[test]
        fn a_correctly_signed_image_is_approved_and_the_swap_stays_requested() {
            let image = image();
            let (mut dfu, mut state) = pending(
                &image,
                PendingImage {
                    len: 600,
                    signature: sign(&image),
                },
            );

            let screening = screen_update(&mut dfu, &mut state, CAPACITY, &public_key());

            assert_eq!(screening.expect("проверка"), Screening::Approved);
            assert_eq!(state_of(&mut state), BootState::Swap);
        }

        #[test]
        fn a_bad_image_is_rejected_and_the_swap_request_is_withdrawn() {
            let image = image();
            let mut tampered = image.clone();
            tampered[10] ^= 1;
            let good = sign(&image);
            let cases = [
                (
                    tampered.clone(),
                    600,
                    good,
                    public_key(),
                    Rejection::Signature,
                ),
                (image.clone(), 0, good, public_key(), Rejection::Length),
                (
                    image.clone(),
                    CAPACITY + 1,
                    good,
                    public_key(),
                    Rejection::Length,
                ),
                (image.clone(), 600, good, [0; 32], Rejection::NoPublicKey),
            ];
            for (bytes, len, signature, key, expected) in cases {
                let (mut dfu, mut state) = pending(&bytes, PendingImage { len, signature });

                let screening = screen_update(&mut dfu, &mut state, CAPACITY, &key);

                assert_eq!(
                    screening.expect("проверка"),
                    Screening::Rejected(expected),
                    "{expected:?}"
                );
                assert_eq!(state_of(&mut state), BootState::Boot, "{expected:?}");
            }
        }

        /// Обмен запросили, минуя запись (образ без подписи), — отказ, а не
        /// «проверять нечего».
        #[test]
        fn a_swap_request_without_a_record_is_rejected() {
            let mut dfu = Dfu::erased();
            let mut state = State::erased();
            BlockingFirmwareState::new(&mut state, &mut [0; 8])
                .mark_updated()
                .expect("запросить обмен");

            let screening = screen_update(&mut dfu, &mut state, CAPACITY, &public_key());

            assert_eq!(
                screening.expect("проверка"),
                Screening::Rejected(Rejection::NoRecord)
            );
            assert_eq!(state_of(&mut state), BootState::Boot);
        }

        #[test]
        fn nothing_is_checked_without_a_swap_request() {
            let mut dfu = Dfu::erased();
            let mut state = State::erased();

            let screening = screen_update(&mut dfu, &mut state, CAPACITY, &public_key());

            assert_eq!(screening.expect("проверка"), Screening::NothingToCheck);
        }

        /// Главное: после настоящего обмена `embassy-boot` магия в `STATE` та же
        /// (`Swap`, пока образ не подтверждён), а запись затёрта. Проверка
        /// обязана это узнать по журналу и не трогать `STATE` — иначе следующий
        /// сброс не откатил бы неподтверждённый образ. Заодно тест держит
        /// привязку к внутреннему формату журнала `embassy-boot`.
        #[test]
        fn after_the_swap_the_pending_revert_is_left_alone() {
            let image = image();
            let (mut dfu, mut state) = pending(
                &image,
                PendingImage {
                    len: 600,
                    signature: sign(&image),
                },
            );
            assert_eq!(
                screen_update(&mut dfu, &mut state, CAPACITY, &public_key()).expect("проверка"),
                Screening::Approved
            );
            let mut active = MemFlash::<768>::filled(0x11);

            let mut boot = BootLoader::new(BootLoaderConfig {
                active: &mut active,
                dfu: &mut dfu,
                state: &mut state,
            });
            assert_eq!(
                boot.prepare_boot(&mut [0; 8]).expect("обмен"),
                BootState::Swap
            );
            assert_eq!(&active.mem[..600], &image[..], "образ переехал в ACTIVE");

            let screening = screen_update(&mut dfu, &mut state, CAPACITY, &public_key());

            assert_eq!(screening.expect("проверка"), Screening::NothingToCheck);
            assert_eq!(
                state_of(&mut state),
                BootState::Swap,
                "откат должен остаться запрошенным"
            );
        }
    }
}
