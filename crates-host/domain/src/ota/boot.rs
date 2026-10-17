//! Решение bootloader'а при старте: обменивать ли разделы и — под фичей
//! `signed` — пускать ли новый образ, проверив его подпись. Над
//! **блокирующими** разделами: у bootloader'а нет исполнителя.

#[cfg(feature = "signed")]
use embassy_boot::AlignedBuffer;
use embassy_boot::BlockingFirmwareState;
#[cfg(feature = "signed")]
use embassy_boot::FirmwareUpdaterError as Error;
#[cfg(feature = "signed")]
use embassy_boot::State;
use ports::MAX_WRITE_SIZE;
#[cfg(feature = "signed")]
use ports::PendingImage;

/// Что bootloader'у делать при старте — см. [`boot_plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootPlan {
    /// `STATE` не читается: загрузить текущий образ, разделы не трогать.
    BootCurrent,
    /// `STATE` читается: строгий `BootLoader::prepare` — обмен, если он
    /// запрошен или прерван посередине; его ошибка обязана кончиться паникой
    /// и сбросом, а не прыжком в `ACTIVE`.
    Prepare,
}

/// Решение bootloader'а с разделами на внешней флеши (`ota=external`):
/// готовить загрузку или сразу грузить текущий образ.
///
/// Внешняя микросхема — отдельная деталь платы (не распаяна, не подставлен
/// драйвер, отказала), и из-за неё устройство не должно перестать
/// загружаться: образ в `ACTIVE` внутренний и, пока обмен его не тронул, цел.
/// Но прыжок в `ACTIVE` посреди обмена — смесь старых и новых страниц:
/// `HardFault`, а дойди такой образ до подтверждения — битый образ навсегда.
/// Отсюда два исхода по `STATE`:
///
/// - не читается — [`BootPlan::BootCurrent`]: был ли начат обмен, узнать
///   нечем; не начатый — частый случай (заглушка, не распаянная микросхема), а
///   отказ флеши посреди обмена, не проходящий и после сброса, — редкий;
/// - читается — [`BootPlan::Prepare`]: строгий `prepare`. Обмен, прерванный
///   отказом, узнаётся по магии `Swap`, которая держится всё время обмена (и
///   отката), и `prepare` его продолжает, а не пропускает.
///
/// `WRITE_SIZE` раздела шире [`MAX_WRITE_SIZE`] не компилируется — на STM32 и
/// распространённых SPI NOR такого нет.
pub fn boot_plan<STATE: embedded_storage::nor_flash::NorFlash>(state: &mut STATE) -> BootPlan {
    const {
        assert!(
            STATE::WRITE_SIZE <= MAX_WRITE_SIZE,
            "слово записи раздела состояния шире MAX_WRITE_SIZE"
        );
    }
    let mut word = [0; MAX_WRITE_SIZE];
    match BlockingFirmwareState::new(state, &mut word[..STATE::WRITE_SIZE]).get_state() {
        Ok(_) => BootPlan::Prepare,
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
    Rejected(ScreenFailure),
}

/// Почему образ в `DFU` отвергнут.
#[cfg(feature = "signed")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenFailure {
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
/// `WRITE_SIZE` раздела `STATE` шире [`MAX_WRITE_SIZE`] не компилируется.
///
/// # Errors
///
/// Ошибка флеша при чтении разделов или при снятии запроса.
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
    const {
        assert!(
            STATE::WRITE_SIZE <= MAX_WRITE_SIZE,
            "слово записи раздела состояния шире MAX_WRITE_SIZE"
        );
    }
    let mut aligned = AlignedBuffer([0; MAX_WRITE_SIZE]);
    let word = &mut aligned.0[..STATE::WRITE_SIZE];
    if BlockingFirmwareState::new(&mut *state, &mut *word).get_state()? != State::Swap {
        return Ok(Screening::NothingToCheck);
    }
    let mut validity = [0; MAX_WRITE_SIZE];
    let mut first_step = [0; MAX_WRITE_SIZE];
    // `WRITE_SIZE <= MAX_WRITE_SIZE` (32) — в `u32` помещается.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "WRITE_SIZE <= MAX_WRITE_SIZE"
    )]
    let size = STATE::WRITE_SIZE as u32;
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
) -> Result<Result<(), ScreenFailure>, Error> {
    /// Сколько байт образа читается за раз на хеш.
    const CHUNK: u32 = 256;
    let mut record = [0; PendingImage::LEN];
    dfu.read(capacity, &mut record)?;
    let Some(pending) = PendingImage::from_bytes(&record) else {
        return Ok(Err(ScreenFailure::NoRecord));
    };
    if pending.len == 0 || pending.len > capacity {
        return Ok(Err(ScreenFailure::Length));
    }
    if public_key.iter().all(|byte| *byte == 0) {
        return Ok(Err(ScreenFailure::NoPublicKey));
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
        return Ok(Err(ScreenFailure::Signature));
    };
    let signature = salty::Signature::from(&pending.signature);
    if key.verify(&message, &signature).is_ok() {
        Ok(Ok(()))
    } else {
        Ok(Err(ScreenFailure::Signature))
    }
}

#[cfg(test)]
mod tests {
    /// Флеш в памяти с семантикой NOR: стирание страницами по 256 байт в
    /// `0xFF`, запись словами по 8 байт и только «единицы в нули». Готового
    /// мока `NorFlash` в экосистеме нет, поэтому фейк свой — блокирующий, как
    /// и сами проверяемые функции.
    mod fakes {
        use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};

        #[derive(Debug)]
        pub(super) struct MemFlash<const SIZE: usize> {
            pub(super) mem: Vec<u8>,
        }

        impl<const SIZE: usize> MemFlash<SIZE> {
            pub(super) fn erased() -> Self {
                Self::filled(0xFF)
            }

            pub(super) fn filled(byte: u8) -> Self {
                Self {
                    mem: vec![byte; SIZE],
                }
            }

            fn check(offset: u32, len: usize, unit: usize) -> Result<(), NorFlashErrorKind> {
                if offset as usize + len > SIZE {
                    return Err(NorFlashErrorKind::OutOfBounds);
                }
                if !(offset as usize).is_multiple_of(unit) || !len.is_multiple_of(unit) {
                    return Err(NorFlashErrorKind::NotAligned);
                }
                Ok(())
            }
        }

        impl<const SIZE: usize> ErrorType for MemFlash<SIZE> {
            type Error = NorFlashErrorKind;
        }

        impl<const SIZE: usize> ReadNorFlash for MemFlash<SIZE> {
            const READ_SIZE: usize = 1;

            fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
                Self::check(offset, bytes.len(), 1)?;
                let start = offset as usize;
                bytes.copy_from_slice(&self.mem[start..start + bytes.len()]);
                Ok(())
            }

            fn capacity(&self) -> usize {
                SIZE
            }
        }

        impl<const SIZE: usize> NorFlash for MemFlash<SIZE> {
            const WRITE_SIZE: usize = 8;
            const ERASE_SIZE: usize = 256;

            fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
                Self::check(from, (to - from) as usize, Self::ERASE_SIZE)?;
                self.mem[from as usize..to as usize].fill(0xFF);
                Ok(())
            }

            fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
                Self::check(offset, bytes.len(), Self::WRITE_SIZE)?;
                for (cell, byte) in self.mem[offset as usize..].iter_mut().zip(bytes) {
                    *cell &= *byte;
                }
                Ok(())
            }
        }

        /// `DFU`, который после `ops` удачных стираний и записей отказывает —
        /// внешняя флеш, отвалившаяся посреди обмена.
        pub(super) struct Flaky {
            pub(super) flash: Dfu,
            pub(super) ops: usize,
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

        /// Четыре страницы по 256 байт, слово 8; ACTIVE (= capacity) — три
        /// страницы, как требует запас на обмен.
        pub(super) type Dfu = MemFlash<1024>;
        /// Раздел состояния: страница на магию плюс журнал прогресса.
        pub(super) type State = MemFlash<256>;

        /// Микросхема, которая не отвечает вовсе (не распаяна, не подставлен
        /// драйвер): всякое чтение — отказ.
        pub(super) struct Unreadable;

        impl ErrorType for Unreadable {
            type Error = NorFlashErrorKind;
        }

        impl ReadNorFlash for Unreadable {
            const READ_SIZE: usize = 1;

            fn read(&mut self, _offset: u32, _bytes: &mut [u8]) -> Result<(), Self::Error> {
                Err(NorFlashErrorKind::Other)
            }

            fn capacity(&self) -> usize {
                0
            }
        }

        impl NorFlash for Unreadable {
            const WRITE_SIZE: usize = 8;
            const ERASE_SIZE: usize = 256;

            fn erase(&mut self, _from: u32, _to: u32) -> Result<(), Self::Error> {
                Err(NorFlashErrorKind::Other)
            }

            fn write(&mut self, _offset: u32, _bytes: &[u8]) -> Result<(), Self::Error> {
                Err(NorFlashErrorKind::Other)
            }
        }
    }

    /// Решение bootloader'а с внешней флешью (`boot_plan`) — над настоящим
    /// обменом `embassy-boot`.
    mod plan {
        use super::fakes::{Dfu, Flaky, MemFlash, State, Unreadable};
        use crate::ota::boot::{BootPlan, boot_plan};
        use embassy_boot::{BlockingFirmwareState, BootLoader, BootLoaderConfig};

        fn requested() -> State {
            let mut state = State::erased();
            BlockingFirmwareState::new(&mut state, &mut [0; 8])
                .mark_updated()
                .expect("запросить обмен");
            state
        }

        #[test]
        fn a_readable_state_without_a_request_is_prepared() {
            assert_eq!(boot_plan(&mut State::erased()), BootPlan::Prepare);
        }

        #[test]
        fn an_unreadable_state_boots_the_current_image() {
            assert_eq!(boot_plan(&mut Unreadable), BootPlan::BootCurrent);
        }

        #[test]
        fn a_requested_swap_is_prepared() {
            assert_eq!(boot_plan(&mut requested()), BootPlan::Prepare);
        }

        /// Главное свойство: обмен, прерванный отказом внешней флеши, —
        /// по-прежнему `Prepare`, то есть строгий `prepare` (сброс и повтор), а
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

            assert_eq!(boot_plan(&mut state), BootPlan::Prepare);

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

        use super::fakes::{Dfu, Flaky, MemFlash, State, Unreadable};
        use crate::ota::boot::{ScreenFailure, Screening, screen_update};

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
                    ScreenFailure::Signature,
                ),
                (image.clone(), 0, good, public_key(), ScreenFailure::Length),
                (
                    image.clone(),
                    CAPACITY + 1,
                    good,
                    public_key(),
                    ScreenFailure::Length,
                ),
                (
                    image.clone(),
                    600,
                    good,
                    [0; 32],
                    ScreenFailure::NoPublicKey,
                ),
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
                Screening::Rejected(ScreenFailure::NoRecord)
            );
            assert_eq!(state_of(&mut state), BootState::Boot);
        }

        /// Обмен, прерванный на середине (журнал уже не пуст), — не новый
        /// запрос: проверять нечего, `embassy-boot` доведёт его сам.
        #[test]
        fn an_interrupted_swap_is_left_alone() {
            let image = image();
            let (dfu, mut state) = pending(
                &image,
                PendingImage {
                    len: 600,
                    signature: sign(&image),
                },
            );
            let mut active = MemFlash::<768>::filled(0x11);
            // Первая страница уже обменена и отмечена в журнале (50 операций с
            // запасом хватает), дальше `DFU` отказывает.
            let mut dfu = Flaky {
                flash: dfu,
                ops: 50,
            };
            let interrupted = BootLoader::new(BootLoaderConfig {
                active: &mut active,
                dfu: &mut dfu,
                state: &mut state,
            })
            .prepare_boot(&mut [0; 8]);
            assert!(interrupted.is_err(), "обмен должен был прерваться");
            assert_eq!(state_of(&mut state), BootState::Swap);

            let screening = screen_update(&mut dfu, &mut state, CAPACITY, &public_key());

            assert_eq!(screening.expect("проверка"), Screening::NothingToCheck);
            assert_eq!(state_of(&mut state), BootState::Swap, "запрос не снят");
        }

        #[test]
        fn unreadable_partitions_are_an_error() {
            let mut state = State::erased();
            assert!(
                screen_update(&mut Dfu::erased(), &mut Unreadable, CAPACITY, &public_key())
                    .is_err(),
                "STATE не читается"
            );
            // Обмен запрошен, а `DFU` не читается — не «проверять нечего».
            BlockingFirmwareState::new(&mut state, &mut [0; 8])
                .mark_updated()
                .expect("запросить обмен");
            assert!(
                screen_update(&mut Unreadable, &mut state, CAPACITY, &public_key()).is_err(),
                "DFU не читается"
            );
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
