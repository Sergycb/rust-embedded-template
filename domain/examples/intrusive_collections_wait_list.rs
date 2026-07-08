//! Intrusive-коллекция: объект встраивает link-поле и может состоять сразу
//! в нескольких коллекциях без отдельного выделения узла на объект.
//! Не дублирует `heapless` (тот копирует значения в коллекцию фиксированной
//! ёмкости) — здесь объекты живут где угодно (на стеке, в `static`), список
//! только ссылается на них через intrusive-линки.
//!
//! `cargo run -p domain --example intrusive_collections_wait_list`

use intrusive_collections::{LinkedList, LinkedListLink, intrusive_adapter};

struct WaitingTask {
    link: LinkedListLink,
    id: u32,
}

intrusive_adapter!(TaskAdapter<'a> = &'a WaitingTask: WaitingTask { link => LinkedListLink });

fn main() {
    let task_a = WaitingTask {
        link: LinkedListLink::new(),
        id: 1,
    };
    let task_b = WaitingTask {
        link: LinkedListLink::new(),
        id: 2,
    };

    let mut wait_list: LinkedList<TaskAdapter<'_>> = LinkedList::new(TaskAdapter::new());
    wait_list.push_back(&task_a);
    wait_list.push_back(&task_b);

    let ids: std::vec::Vec<u32> = wait_list.iter().map(|task| task.id).collect();
    assert_eq!(ids, std::vec![1, 2]);
}
