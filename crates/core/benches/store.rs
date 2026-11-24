//! 一覧の読み出しなど、画面の速さに効く操作のベンチマーク。

use criterion::{criterion_group, criterion_main, Criterion};
use malakt_core::{Store, TaskDraft, TaskFilter};

fn seeded(tasks: usize) -> (Store, String) {
    let mut store = Store::open_in_memory().unwrap();
    let root = store.create_workspace(None, "root").unwrap();
    let child = store.create_workspace(Some(&root.id), "child").unwrap();
    let me = store.create_person("me", true).unwrap();
    for i in 0..tasks {
        let ws = if i % 2 == 0 { &root.id } else { &child.id };
        let draft = TaskDraft {
            title: format!("タスク {i}"),
            description: "説明 ".repeat(8),
            assignee_ids: if i % 3 == 0 {
                vec![me.id.clone()]
            } else {
                vec![]
            },
            ..Default::default()
        };
        store.create_task(Some(ws), &draft).unwrap();
    }
    (store, root.id)
}

fn bench(c: &mut Criterion) {
    let (store, root) = seeded(10_000);
    c.bench_function("tasks/10k/workspace", |b| {
        b.iter(|| store.tasks(&root, &TaskFilter::default()).unwrap())
    });
    c.bench_function("tasks/10k/descendants", |b| {
        let filter = TaskFilter {
            include_descendants: true,
            ..Default::default()
        };
        b.iter(|| store.tasks(&root, &filter).unwrap())
    });
    c.bench_function("tasks/10k/search", |b| {
        let filter = TaskFilter {
            include_descendants: true,
            query: Some("タスク 99".into()),
            ..Default::default()
        };
        b.iter(|| store.tasks(&root, &filter).unwrap())
    });
    c.bench_function("open_task_counts/10k", |b| {
        b.iter(|| store.open_task_counts().unwrap())
    });
    c.bench_function("export/10k", |b| {
        b.iter(|| {
            let mut buf = std::io::Cursor::new(Vec::new());
            malakt_core::archive::export(&store, &mut buf).unwrap();
            buf
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench
}
criterion_main!(benches);
