//! Иерархический стейтчарт как отдельная "задача" через `hsmc`. В отличие
//! от `statig` (компонент внутри произвольной задачи, вызывающий код сам
//! кормит события через `handle()`), `hsmc` — модель "весь `run()` — это
//! вся задача целиком", с таймерами и cross-task-инъекцией событий (через
//! `Sender`, полученный из статического `Channel`) встроенными прямо в
//! декларацию состояний. `embassy`-фича (no_std) требует явного
//! статического канала — здесь, как и в реальной прошивке,
//! `CriticalSectionRawMutex` (для `static` нужен реальный `Sync`, не
//! `NoopRawMutex`, тот подходит только для не-static/локальных случаев).
//!
//! `cargo run -p domain --example hsmc_connection_chart`

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use hsmc::{Duration, statechart};

#[derive(Debug, Clone)]
pub enum ConnEv {
    Connect,
    Connected,
    Disconnect,
    Shutdown,
}

pub struct ConnCtx {
    pub reconnects: u32,
}

statechart! {
    Connection {
        context: ConnCtx;
        events: ConnEv;
        terminate(Shutdown);
        default(Disconnected);

        state Disconnected {
            on(Connect) => Connecting;
        }
        state Connecting {
            on(Connected) => Connected;
            on(after Duration::from_millis(50)) => Disconnected;
        }
        state Connected {
            entry: on_connected;
            on(Disconnect) => Disconnected;
        }
    }
}

impl ConnectionActions for ConnectionActionContext<'_> {
    async fn on_connected(&mut self) {
        self.reconnects += 1;
    }
}

// Ёмкость канала должна совпадать с внутренней emit-очередью машины (8).
const QN: usize = 8;
static CONN_CHAN: Channel<CriticalSectionRawMutex, ConnEv, QN> = Channel::new();

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut machine = Connection::new(ConnCtx { reconnects: 0 }, &CONN_CHAN);

    tokio::spawn(async move {
        CONN_CHAN.send(ConnEv::Connect).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        CONN_CHAN.send(ConnEv::Connected).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        CONN_CHAN.send(ConnEv::Shutdown).await;
    });

    let res = tokio::time::timeout(Duration::from_secs(2), machine.run()).await;
    res.expect("run() hung past timeout")
        .expect("chart terminated with an error");

    let ctx = machine.into_context();
    assert_eq!(ctx.reconnects, 1);
}
