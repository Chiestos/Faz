use crate::domain::{alloc_domain_id, Domain, DomainId, DomainState, SplitSource};
use crate::pane::{Pane, PaneId};
use crate::tab::{SplitRequest, Tab, TabId};
use crate::tmux_commands::{
    ListAllPanes, ListAllWindows, ListCommands, NewWindow, SplitPane, TmuxCommand,
};
use crate::window::WindowId;
use crate::{Mux, MuxWindowBuilder};
use async_trait::async_trait;
use config::keyassignment::PaneEncoding;
use filedescriptor::FileDescriptor;
use parking_lot::{Condvar, Mutex};
use portable_pty::CommandBuilder;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::sync::Arc;
use termwiz::tmux_cc::*;
use wezterm_term::TerminalSize;

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum AttachState {
    Init,
    Done,
}

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
enum State {
    WaitForInitialGuard,
    Idle,
    WaitingForResponse,
    Exit,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct TmuxRemotePane {
    // members for local
    pub local_pane_id: PaneId,
    pub output_write: FileDescriptor,
    pub active_lock: Arc<(Mutex<bool>, Condvar)>,
    // members sync with remote
    pub session_id: TmuxSessionId,
    pub window_id: TmuxWindowId,
    pub pane_id: TmuxPaneId,
    pub cursor_x: u64,
    pub cursor_y: u64,
    pub pane_width: u64,
    pub pane_height: u64,
    pub pane_left: u64,
    pub pane_top: u64,
}

pub(crate) type RefTmuxRemotePane = Arc<Mutex<TmuxRemotePane>>;

/// As a remote TmuxTab, keeping the TmuxPanes ID
/// within the remote tab.
#[allow(dead_code)]
pub(crate) struct TmuxTab {
    pub tab_id: TabId, // local tab ID
    pub tmux_window_id: TmuxWindowId,
    pub layout_csum: String,
    pub panes: HashSet<TmuxPaneId>, // tmux panes within tmux window
}

pub(crate) type TmuxCmdQueue = VecDeque<Box<dyn TmuxCommand>>;
pub(crate) struct TmuxDomainState {
    pub pane_id: PaneId,     // ID of the original pane
    pub domain_id: DomainId, // ID of TmuxDomain
    /// Unique name for this tmux session's domain.
    ///
    /// Every `tmux -CC` gets its own domain and window, so a fixed "tmux"
    /// name would make them collide in the mux's name index.
    pub name: String,
    state: Mutex<State>,
    pub cmd_queue: Arc<Mutex<TmuxCmdQueue>>,
    /// The window hosting this tmux session's tabs.
    ///
    /// Holds the builder until the first tab lands, then drops it: dropping
    /// fires `MuxNotification::WindowCreated`, which is what makes the GUI
    /// adopt the window. Keeping the builder alive forever (the previous
    /// behaviour) meant the notification never fired and the window never
    /// appeared.
    pub gui_window: Mutex<Option<MuxWindowBuilder>>,
    /// Set once the builder has been dropped and the GUI was told about the
    /// window. `gui_window` is `None` from then on; use this for the id.
    pub gui_window_id: Mutex<Option<WindowId>>,
    pub gui_tabs: Mutex<HashMap<TmuxWindowId, TmuxTab>>,
    pub remote_panes: Mutex<HashMap<TmuxPaneId, RefTmuxRemotePane>>,
    pub tmux_session: Mutex<Option<TmuxSessionId>>,
    pub support_commands: Mutex<HashMap<String, String>>,
    pub attach_state: Mutex<AttachState>,
    pending_splits: Mutex<VecDeque<promise::Promise<TmuxPaneId>>>,
    /// Tabs requested through `Domain::spawn` that are waiting for tmux to
    /// answer with `%window-add`. Resolved by `sync_window_state` once the
    /// tab exists locally.
    pub(crate) pending_spawns: Mutex<VecDeque<promise::Promise<Arc<Tab>>>>,
    pub backlog: Mutex<HashMap<TmuxPaneId, Vec<u8>>>,
}

pub struct TmuxDomain {
    pub(crate) inner: Arc<TmuxDomainState>,
}

impl TmuxDomainState {
    pub fn advance(&self, events: Box<Vec<Event>>) {
        for event in events.iter() {
            let state = *self.state.lock();
            log::debug!("tmux: {:?} in state {:?}", event, state);
            match event {
                // Tmux generic events
                Event::Guarded(response) => match state {
                    State::WaitForInitialGuard => {
                        *self.state.lock() = State::Idle;
                    }
                    State::WaitingForResponse => {
                        let mut cmd_queue = self.cmd_queue.as_ref().lock();
                        if let Some(cmd) = cmd_queue.pop_front() {
                            let domain_id = self.domain_id;
                            *self.state.lock() = State::Idle;
                            let resp = response.clone();
                            promise::spawn::spawn_into_main_thread(async move {
                                if let Err(err) = cmd.process_result(domain_id, &resp) {
                                    log::error!("Tmux processing command result error: {}", err);
                                }
                            })
                            .detach();
                        }
                    }
                    State::Idle => {}
                    State::Exit => {}
                },

                // Tmux specific events
                Event::ConfigError { error } => {
                    // tmux config file error, not our fault, just log it and go
                    log::warn!("tmux configuration error: {error}");
                }
                Event::Exit { reason: _ } => {
                    *self.state.lock() = State::Exit;
                    let mut pane_map = self.remote_panes.lock();
                    for (_, v) in pane_map.iter_mut() {
                        let remote_pane = v.lock();
                        let (lock, condvar) = &*remote_pane.active_lock;
                        let mut released = lock.lock();
                        *released = true;
                        condvar.notify_all();
                    }
                    let mut cmd_queue = self.cmd_queue.as_ref().lock();
                    cmd_queue.clear();

                    // Force to quit the tmux mode
                    let pane_id = self.pane_id;
                    promise::spawn::spawn_into_main_thread_with_low_priority(async move {
                        if let Some(x) = Mux::get().get_pane(pane_id) {
                            let _ = write!(x.writer(), "\n\n");
                        }
                    })
                    .detach();

                    return;
                }
                Event::LayoutChange {
                    window,
                    layout,
                    visible_layout: _,
                    raw_flags: _,
                } => {
                    let mut cmd_queue = self.cmd_queue.as_ref().lock();
                    cmd_queue.push_back(Box::new(ListAllPanes {
                        window_id: *window,
                        prune: true,
                        layout_csum: if let Some(l) = layout.get(0..4) {
                            l.to_string()
                        } else {
                            "".to_string()
                        },
                    }));
                }
                Event::Output { pane, text } => {
                    let pane_map = self.remote_panes.lock();
                    if let Some(ref_pane) = pane_map.get(pane) {
                        let mut tmux_pane = ref_pane.lock();
                        if let Err(err) = tmux_pane.output_write.write_all(text) {
                            log::error!("Failed to write tmux data to output: {:#}", err);
                        }
                    } else {
                        // the output may come early then pane is ready, in this case we
                        // backlog it
                        self.backlog.lock().insert(*pane, text.to_vec());
                        log::debug!("Tmux pane {} havn't been attached", pane);
                    }
                }
                Event::SessionChanged { session, name: _ } => {
                    *self.tmux_session.lock() = Some(*session);
                    let mut cmd_queue = self.cmd_queue.as_ref().lock();
                    cmd_queue.push_back(Box::new(ListCommands));

                    self.subscribe_notification();
                    log::info!("tmux session changed:{}", session);
                }
                // Only handle the new tab, the first empty window handled by sync_window_state
                Event::WindowAdd { window }
                    if self.gui_window.lock().is_some() || self.gui_window_id.lock().is_some() =>
                {
                    if let Some(session) = *self.tmux_session.lock() {
                        let mut cmd_queue = self.cmd_queue.as_ref().lock();
                        cmd_queue.push_back(Box::new(ListAllWindows {
                            session_id: session,
                            window_id: Some(*window),
                        }));
                        log::info!("tmux window add: {}:{}", session, window);
                    }
                }
                Event::WindowClose { window } => {
                    let _ = self.remove_detached_window(*window);
                }
                Event::WindowPaneChanged { window, pane } => {
                    // The tmux 2.7 WindowPaneChanged event comes early than WindowAdd, we need to
                    // skip it
                    if !self.check_window_attached(*window) {
                        continue;
                    }

                    // Split pane
                    if !self.check_pane_attached(*window, *pane) {
                        let mut pending_splits = self.pending_splits.lock();
                        if let Some(mut promise) = pending_splits.pop_front() {
                            promise.ok(*pane);
                        }
                    }
                    log::info!("tmux window pane changed: {}:{}", window, pane);
                }
                Event::WindowRenamed { window, name } => {
                    let gui_tabs = self.gui_tabs.lock();
                    if let Some(x) = gui_tabs.get(&window) {
                        let mux = Mux::get();
                        if let Some(tab) = mux.get_tab(x.tab_id) {
                            tab.set_title(&format!("{}", name));
                        }
                    }
                }
                Event::UnlinkedWindowClose { window } => {
                    let _ = self.remove_detached_window(*window);
                }
                _ => {}
            }
        }

        // send pending commands to tmux
        let cmd_queue = self.cmd_queue.as_ref().lock();
        if *self.state.lock() == State::Idle && !cmd_queue.is_empty() {
            TmuxDomainState::schedule_send_next_command(self.domain_id);
        }
    }

    /// send next command at the front of cmd_queue.
    /// must be called inside main thread
    fn send_next_command(&self) {
        if *self.state.lock() != State::Idle {
            return;
        }
        let mut cmd_queue = self.cmd_queue.as_ref().lock();
        while let Some(first) = cmd_queue.front() {
            let cmd = first.get_command(self.domain_id);
            if cmd.is_empty() {
                cmd_queue.pop_front();
                continue;
            }
            log::debug!("sending cmd {:?}", cmd);
            let mux = Mux::get();
            if let Some(pane) = mux.get_pane(self.pane_id) {
                let mut writer = pane.writer();
                let _ = write!(writer, "{}", cmd);
            }
            *self.state.lock() = State::WaitingForResponse;
            break;
        }
    }

    /// schedule a `send_next_command` into main thread
    pub fn schedule_send_next_command(domain_id: DomainId) {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            if let Some(domain) = mux.get_domain(domain_id) {
                if let Some(tmux_domain) = domain.downcast_ref::<TmuxDomain>() {
                    tmux_domain.send_next_command();
                }
            }
        })
        .detach();
    }

    /// create a standalone window for tmux tabs
    pub fn create_gui_window(&self) {
        if self.gui_window.lock().is_some() || self.gui_window_id.lock().is_some() {
            return;
        }

        // Always a fresh window: the pane running `tmux -CC` keeps its own
        // window untouched, and every tmux session gets a window of its own.
        let mux = Mux::get();
        let window_builder = mux.new_empty_window(
            None, /* TODO: pass session here */
            None, /* position */
        );

        log::info!("Tmux create window id {}", window_builder.window_id);
        *self.gui_window.lock() = Some(window_builder);
    }

    /// The tmux window's id, once one has been created.
    pub fn gui_window_id(&self) -> Option<WindowId> {
        if let Some(id) = *self.gui_window_id.lock() {
            return Some(id);
        }
        self.gui_window.lock().as_ref().map(|b| b.window_id)
    }

    /// Drop the window builder once the first tab is in place.
    ///
    /// This fires `WindowCreated`, so the GUI adopts a window that already
    /// has content. Call it only after `add_tab_to_window`.
    pub fn publish_gui_window(&self) {
        let builder = self.gui_window.lock().take();
        if let Some(mut builder) = builder {
            let window_id = builder.window_id;
            *self.gui_window_id.lock() = Some(window_id);
            builder.notify();
        }
    }

    /// create a tmux window
    pub fn create_tmux_window(&self) {
        let mut cmd_queue = self.cmd_queue.as_ref().lock();
        cmd_queue.push_back(Box::new(NewWindow));
        TmuxDomainState::schedule_send_next_command(self.domain_id);
    }

    /// split the tmux pane
    pub fn split_tmux_pane(
        &self,
        _tab: TabId,
        pane_id: PaneId,
        split_request: SplitRequest,
    ) -> anyhow::Result<()> {
        let tmux_pane_id = self
            .remote_panes
            .lock()
            .iter()
            .find(|(_, ref_pane)| ref_pane.lock().local_pane_id == pane_id)
            .map(|p| p.1.lock().pane_id);

        if let Some(id) = tmux_pane_id {
            let mut cmd_queue = self.cmd_queue.as_ref().lock();
            cmd_queue.push_back(Box::new(SplitPane {
                pane_id: id,
                direction: split_request.direction,
            }));
            TmuxDomainState::schedule_send_next_command(self.domain_id);
            return Ok(());
        } else {
            anyhow::bail!("Could not find the tmux pane peer for local pane: {pane_id}");
        }
    }
}

impl TmuxDomain {
    pub fn new(pane_id: PaneId) -> Self {
        let domain_id = alloc_domain_id();
        let cmd_queue = VecDeque::new();
        let inner = Arc::new(TmuxDomainState {
            domain_id,
            name: format!("tmux-{pane_id}"),
            pane_id,
            // parser,
            state: Mutex::new(State::WaitForInitialGuard),
            cmd_queue: Arc::new(Mutex::new(cmd_queue)),
            gui_window: Mutex::new(None),
            gui_window_id: Mutex::new(None),
            gui_tabs: Mutex::new(HashMap::default()),
            remote_panes: Mutex::new(HashMap::default()),
            tmux_session: Mutex::new(None),
            support_commands: Mutex::new(HashMap::default()),
            attach_state: Mutex::new(AttachState::Init),
            pending_splits: Mutex::new(VecDeque::default()),
            pending_spawns: Mutex::new(VecDeque::default()),
            backlog: Mutex::new(HashMap::default()),
        });

        Self { inner }
    }

    fn send_next_command(&self) {
        self.inner.send_next_command();
    }
}

#[async_trait(?Send)]
impl Domain for TmuxDomain {
    async fn spawn(
        &self,
        _mux: &Mux,
        _size: TerminalSize,
        _command: Option<CommandBuilder>,
        _command_dir: Option<String>,
        _encoding: PaneEncoding,
        _window: WindowId,
    ) -> anyhow::Result<Arc<Tab>> {
        // tmux owns the tab: ask it for a new window and wait for the
        // resulting `%window-add` to be turned into a local tab by
        // `sync_window_state`.
        let mut promise = promise::Promise::new();
        let future = promise
            .get_future()
            .ok_or_else(|| anyhow::anyhow!("promise already taken"))?;
        self.inner.pending_spawns.lock().push_back(promise);
        self.inner.create_tmux_window();

        // If tmux never answers (rejected command, session gone), fail
        // instead of leaving the caller's spawn pending forever.
        let timeout = async {
            smol::Timer::after(std::time::Duration::from_secs(10)).await;
            Err(anyhow::anyhow!(
                "timed out waiting for tmux to create a new window"
            ))
        };
        smol::future::or(future, timeout).await.map_err(|err| {
            match err.downcast::<promise::BrokenPromise>() {
                Ok(_) => anyhow::anyhow!("tmux closed before the new window could be created"),
                Err(err) => err.context("waiting for tmux to create a new window"),
            }
        })
    }

    fn spawnable(&self) -> bool {
        true
    }

    async fn split_pane(
        &self,
        _mux: &Mux,
        _source: SplitSource,
        tab: TabId,
        pane_id: PaneId,
        split_request: SplitRequest,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        let mut promise = promise::Promise::new();
        if let Some(future) = promise.get_future() {
            {
                let mut pending_splits = self.inner.pending_splits.lock();
                let _ = self.inner.split_tmux_pane(tab, pane_id, split_request)?;
                pending_splits.push_back(promise);
            }

            if let Ok(id) = future.await {
                let pane = self.inner.split_pane(tab, pane_id, id, split_request);
                return pane;
            }
        }

        anyhow::bail!("Split_pane failed");
    }

    async fn spawn_pane(
        &self,
        _mux: &Mux,
        _size: TerminalSize,
        _command: Option<CommandBuilder>,
        _command_dir: Option<String>,
        _encoding: PaneEncoding,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        anyhow::bail!("Spawn_pane not yet implemented for TmuxDomain");
    }

    fn domain_id(&self) -> DomainId {
        self.inner.domain_id
    }

    fn domain_name(&self) -> &str {
        &self.inner.name
    }

    async fn attach(&self, _window_id: Option<crate::WindowId>) -> anyhow::Result<()> {
        Ok(())
    }

    fn detachable(&self) -> bool {
        // Closing the window that hosts this session should end the
        // `tmux -CC` client, just like `c-b d`: the tmux session keeps
        // running on the server, but Faz lets go of it.
        true
    }

    fn detach(&self) -> anyhow::Result<()> {
        let mux = Mux::get();
        let Some(pane) = mux.get_pane(self.inner.pane_id) else {
            // The pane running `tmux -CC` is already gone; nothing to send.
            return Ok(());
        };
        // Ask the pane to leave control mode. It must bypass the tmux input
        // interception, which would otherwise forward this to a remote pane
        // instead of the tmux client. Leaving control mode closes the DCS and
        // lands in `DeviceControlMode::Exit`, where the local domain and tabs
        // are cleaned up while the tmux session keeps running on the server.
        let local = pane
            .downcast_ref::<crate::localpane::LocalPane>()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "tmux control pane {} is not a LocalPane",
                    self.inner.pane_id
                )
            })?;
        local.detach_tmux()?;
        Ok(())
    }

    fn state(&self) -> DomainState {
        DomainState::Attached
    }
}
