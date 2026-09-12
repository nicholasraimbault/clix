use clix::{tray_tooltip, BodyId, Job, JobStatus, Request, Store};

fn empty_store() -> Store {
    let dir = tempfile::tempdir().unwrap();
    Store::open(dir.path()).unwrap()
}

fn store_with_one_request() -> Store {
    let mut store = empty_store();
    store.requests.push(Request {
        id: "req-1".into(),
        from: BodyId("server".into()),
        tool: "true".into(),
        once_suggested: true,
    });
    store
}

fn store_with_running_job() -> Store {
    let mut store = empty_store();
    store.jobs.push(Job {
        id: "job-1".into(),
        body: BodyId("laptop".into()),
        argv: vec!["true".into()],
        from: BodyId("server".into()),
        status: JobStatus::Running,
    });
    store
}

#[test]
fn tooltip_pending_and_running() {
    assert_eq!(tray_tooltip(&store_with_one_request()), "1 request");
    assert_eq!(tray_tooltip(&store_with_running_job()), "true from server");
    assert_eq!(tray_tooltip(&empty_store()), "Clix is running");
}
