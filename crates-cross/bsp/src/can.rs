#![doc = include_str!("../../../docs/modules/bsp-can.md")]

use embassy_stm32::can::enums::BusError;
use embassy_stm32::can::{Can as Controller, Frame};
use embassy_time::{Duration, Instant, TimeoutError, Timer, with_timeout};
use ports::{CanBus, LinkError};

/// Шина CAN платы — пока заглушка, до контроллера (см. модуль).
pub type Can = adapters::can::Unconnected<Frame>;

/// Собирает шину CAN платы.
pub(crate) fn new() -> Can {
    defmt::warn!("bsp: CAN не подключён — узел CANopen ждёт впустую");
    Can::default()
}

/// Сколько ждать места в очереди передачи контроллера.
///
/// Драйвер `embassy-stm32` ждёт свободный слот сколько угодно, а на шине, где
/// кадр никто не подтверждает (плата одна, трансивер обесточен, bus-off),
/// слот не освободится никогда — и узел CANopen встал бы внутри отправки,
/// не замечая ни такта, ни приёма. Кадр 8 байт на 125 кбит/с уходит за
/// ~1 мс, очередь — три кадра: 5 мс с запасом хватает на живой шине.
const SEND_TIMEOUT: Duration = Duration::from_millis(5);

/// Пауза после ошибки шины на приёме: следующий приём начинается с неё.
///
/// Порт обязан переждать отказ сам (`ports::CanBus`): контроллер в bus-off
/// или на линии с помехой отвечает ошибкой на каждое чтение без ожидания, и
/// узел крутил бы цикл, не отдавая исполнитель. Ошибка возвращается сразу, а
/// ждёт следующий вызов: пауза длиннее такта узла CANopen, и такт, отменив
/// `recv` посреди паузы, иначе ни разу не дал бы ошибке дойти до лога. 50 мс
/// — короткий сбой ловится почти сразу, а выход из bus-off (128 × 11
/// рецессивных бит; на 10 кбит/с — 141 мс) занимает несколько пауз, а не
/// тысячи чтений.
const ERROR_PAUSE: Duration = Duration::from_millis(50);
{%- if can_kind == "bxcan" %}

/// Сколько ждать синхронизации bxCAN с линией при включении (11
/// рецессивных бит; на 10 кбит/с — 1,1 мс). Включение `embassy-stm32`
/// ждёт её в цикле `yield_now` без сна, и на линии, застрявшей в
/// доминантном уровне, исполнитель не уснул бы вовсе; с пределом это
/// потеря кадра, а не зависший узел.
const ENABLE_TIMEOUT: Duration = Duration::from_millis(5);
{%- endif %}

/// Почему [`Bus`] не принял или не отправил кадр. Каждый случай — потеря
/// кадра ([`LinkError::Lost`]): шина та же, следующий может пройти. Сразу
/// снова ошибкой шина не ответит: приём после ошибки ждёт [`ERROR_PAUSE`],
/// отправка — до [`SEND_TIMEOUT`].
#[derive(Debug, defmt::Format)]
pub enum CanError {
    /// Ошибка шины на приёме: stuff, form, CRC, нет ACK.
    Bus(BusError),
    /// Очередь передачи не освободилась за [`SEND_TIMEOUT`]: кадры никто не
    /// подтверждает.
    Busy,
    /// Кадр поставлен в очередь, но вытеснил из неё менее приоритетный —
    /// тот потерян.
    Displaced,
{%- if can_kind == "bxcan" %}
    /// Контроллер не синхронизировался с линией за [`ENABLE_TIMEOUT`].
    Offline,
{%- endif %}
}

/// Порт [`CanBus`] поверх CAN-контроллера `embassy-stm32` ({% if can_kind == "fdcan" %}FDCAN{% else %}bxCAN{% endif %}
/// у этого чипа): то, на что меняется заглушка [`Can`], когда у платы
/// появились выводы.
pub struct Bus {
    can: Controller<'static>,
    /// До какого момента приём пережидает последнюю ошибку шины.
    paused_until: Option<Instant>,
{%- if can_kind == "bxcan" %}
    /// Включён ли контроллер: у bxCAN выход из режима инициализации —
    /// `async` (ждёт синхронизации с линией), а `Board::new` — синхронный,
    /// поэтому включение — при первом обмене.
    enabled: bool,
{%- endif %}
}

// Вручную: у контроллера `embassy-stm32` нет `Debug`, а без него
// `missing_debug_implementations` — ошибка в `lint cross`.
impl core::fmt::Debug for Bus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bus").finish_non_exhaustive()
    }
}

impl Bus {
    /// Шина поверх сконфигурированного контроллера
    /// ({% if can_kind == "fdcan" %}`CanConfigurator::into_normal_mode`{% else %}`Can::new` + `set_bitrate`; включит его первый обмен{% endif %}).
    #[must_use]
    pub const fn new(can: Controller<'static>) -> Self {
{%- if can_kind == "bxcan" %}
        Self {
            can,
            paused_until: None,
            enabled: false,
        }
{%- else %}
        Self {
            can,
            paused_until: None,
        }
{%- endif %}
    }
{%- if can_kind == "bxcan" %}

    async fn enable(&mut self) -> Result<(), LinkError<CanError>> {
        if !self.enabled {
            with_timeout(ENABLE_TIMEOUT, self.can.enable())
                .await
                .map_err(|TimeoutError| LinkError::Lost(CanError::Offline))?;
            self.enabled = true;
        }
        Ok(())
    }
{%- endif %}
}

impl CanBus for Bus {
    type Frame = Frame;
    type Error = CanError;

    async fn recv(&mut self) -> Result<Frame, LinkError<CanError>> {
{%- if can_kind == "bxcan" %}
        self.enable().await?;
{%- endif %}
        if let Some(until) = self.paused_until {
            // Сброс — после паузы: `recv`, отменённый посреди неё, начнёт
            // её заново, а не пропустит.
            Timer::at(until).await;
            self.paused_until = None;
        }
        match self.can.read().await {
            Ok(envelope) => Ok(envelope.frame),
            Err(err) => {
                self.paused_until = Some(Instant::now() + ERROR_PAUSE);
                Err(LinkError::Lost(CanError::Bus(err)))
            }
        }
    }

    async fn send(&mut self, frame: &Frame) -> Result<(), LinkError<CanError>> {
{%- if can_kind == "bxcan" %}
        self.enable().await?;
        let status = with_timeout(SEND_TIMEOUT, self.can.write(frame))
            .await
            .map_err(|TimeoutError| LinkError::Lost(CanError::Busy))?;
        match status.dequeued_frame() {
            None => Ok(()),
            Some(_) => Err(LinkError::Lost(CanError::Displaced)),
        }
{%- else %}
        match with_timeout(SEND_TIMEOUT, self.can.write(frame)).await {
            Ok(None) => Ok(()),
            Ok(Some(_)) => Err(LinkError::Lost(CanError::Displaced)),
            Err(TimeoutError) => Err(LinkError::Lost(CanError::Busy)),
        }
{%- endif %}
    }
}
