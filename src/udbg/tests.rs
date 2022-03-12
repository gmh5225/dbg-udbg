
use super::*;

#[derive(AsRef)]
struct MyDebugger(ShellData);

impl UDbgShell for MyDebugger {}

#[test]
fn main() -> anyhow::Result<()> {
    set_ui(MyDebugger(ShellData::default()));
    flexi_logger::Logger::try_with_env_or_str("info")?.use_utc().start()?;

    let dbg = DefaultEngine.create(Default::default(), r"C:\Windows\System32\notepad.exe", None, &[]).unwrap();
    // dbg.event_loop(&mut |event| {
    //     println!("[event]~{} {event}", dbg.base().event_tid.get());
    //     match event {
    //         UEvent::Exception{..} => UserReply::Run(false),
    //         _ => UserReply::Run(true),
    //     }
    // })?;
    dbg.loop_event(|dbg, state| async move {
        while let Some(event) = state.cont(UserReply::Run(false)).await {
            println!("[event]~{} {event}", dbg.base().event_tid.get());
        }
    });

    Ok(())
}