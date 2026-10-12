use std::{
    fs::File,
    path::Path,
    time::{Duration, Instant},
};

use nix::{
    sys::wait::{waitpid, WaitPidFlag, WaitStatus},
    unistd::alarm,
};
use tokio::{sync::RwLock, time::sleep};

use crate::{
    config::SessionVt,
    error::Error,
    scrambler::Scrambler,
    session::{
        interface::{Session, SessionChild, SessionState},
        worker::{AuthMessageType as SessAuthMessageType, SessionClass, TerminalMode},
    },
    terminal::Terminal,
};

/// WMDE: how long a session started on a VT of its own gets to take that VT
/// over before greetd switches to it anyway.
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(10);
/// WMDE: how often the session's VT is checked during a handover.
const HANDOVER_POLL: Duration = Duration::from_millis(50);
use greetd_ipc::AuthMessageType;

struct SessionChildSet {
    child: SessionChild,
    time: Instant,
    is_greeter: bool,
}

struct SessionSet {
    session: Session,
    time: Instant,
    /// WMDE: the VT this session was set up on, when it is not the greeter's.
    own_vt: Option<(String, usize)>,
}

/// WMDE: a user session started on a VT of its own while the greeter, still
/// `current`, keeps the screen.
struct Handover {
    child: SessionChildSet,
    path: String,
    vt: usize,
}

struct ContextInner {
    current: Option<SessionChildSet>,
    scheduled: Option<SessionSet>,
    configuring: Option<SessionSet>,
    /// WMDE: the session waiting to take the screen over from the greeter.
    handover: Option<Handover>,
    /// WMDE: the greeter after a handover, being asked to exit.
    retiring: Option<SessionChildSet>,
    /// WMDE: the greeter is being ended because the session it handed over to
    /// died first; start a new one when it is gone.
    restart_greeter: bool,
}

/// Context keeps track of running sessions and start new ones.
pub struct Context {
    inner: RwLock<ContextInner>,
    greeter_bin: String,
    greeter_user: String,
    greeter_service: String,
    pam_service: String,
    term_mode: TerminalMode,
    session_vt: SessionVt,
    source_profile: bool,
    runfile: String,
    listener_path: String,
}

impl Context {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        greeter_bin: String,
        greeter_user: String,
        greeter_service: String,
        pam_service: String,
        term_mode: TerminalMode,
        session_vt: SessionVt,
        source_profile: bool,
        runfile: String,
        listener_path: String,
    ) -> Context {
        Context {
            inner: RwLock::new(ContextInner {
                current: None,
                scheduled: None,
                configuring: None,
                handover: None,
                retiring: None,
                restart_greeter: false,
            }),
            greeter_bin,
            greeter_user,
            greeter_service,
            pam_service,
            term_mode,
            session_vt,
            source_profile,
            runfile,
            listener_path,
        }
    }

    /// Directly start an unauthenticated session, bypassing the normal
    /// scheduling. This function does not take the inner lock, and can thus
    /// be used while it is held.
    async fn start_unauthenticated_session(
        &self,
        class: SessionClass,
        user: &str,
        service: &str,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<SessionChild, Error> {
        let mut scheduled_session = Session::new_external()?;
        scheduled_session
            .initiate(
                service,
                class,
                user,
                false,
                &self.term_mode,
                self.source_profile,
                &self.listener_path,
            )
            .await?;
        loop {
            match scheduled_session.get_state().await {
                Ok(SessionState::Ready) => break,
                Ok(SessionState::Question(_, _)) => scheduled_session.post_response(None).await?,
                Err(err) => return Err(format!("session start failed: {err}").into()),
            }
        }

        scheduled_session.send_args(cmd, env).await?;
        scheduled_session.start().await
    }

    /// Directly start a greeter session, bypassing the normal scheduling. This
    /// function does not take the inner lock, and can thus be used while it is
    /// held.
    async fn start_greeter(&self) -> Result<SessionChild, Error> {
        // WMDE: tell the greeter that greetd ends it after a handover, so it
        // keeps running once its session has started instead of exiting.
        let env = match self.session_vt {
            SessionVt::Next => vec!["GREETD_HANDOVER=1".to_string()],
            SessionVt::Same => vec![],
        };
        self.start_unauthenticated_session(
            SessionClass::Greeter,
            &self.greeter_user,
            &self.greeter_service,
            vec![self.greeter_bin.to_string()],
            env,
        )
        .await
    }

    /// Directly start a greeter session, bypassing the normal scheduling.
    pub async fn greet(&self) -> Result<(), Error> {
        {
            let inner = self.inner.read().await;
            if inner.current.is_some() {
                return Err("session already active".into());
            }
        }

        let mut inner = self.inner.write().await;
        inner.current = Some(SessionChildSet {
            child: self.start_greeter().await?,
            time: Instant::now(),
            is_greeter: true,
        });
        Ok(())
    }

    /// Check if this is the first time greetd starts since boot, or if it restarted for any reason
    pub fn is_first_run(&self) -> bool {
        !Path::new(&self.runfile).exists()
    }

    /// Create runfile used to check if greetd was already started since boot
    pub fn create_runfile(&self) {
        if let Err(err) = File::create(&self.runfile) {
            eprintln!("could not create runfile: {err}");
        }
    }

    /// Directly start an initial session, bypassing the normal scheduling.
    pub async fn start_user_session(
        &self,
        user: &str,
        service: &str,
        cmd: Vec<String>,
    ) -> Result<(), Error> {
        {
            let inner = self.inner.read().await;
            if inner.current.is_some() {
                return Err("session already active".into());
            }
        }

        let mut inner = self.inner.write().await;
        inner.current = Some(SessionChildSet {
            child: self
                .start_unauthenticated_session(SessionClass::User, user, service, cmd, vec![])
                .await?,
            time: Instant::now(),
            is_greeter: false,
        });
        Ok(())
    }

    /// Create a new session for configuration.
    pub async fn create_session(&self, username: String) -> Result<(), Error> {
        {
            let inner = self.inner.read().await;
            if inner.current.is_none() {
                return Err("session not active".into());
            }
            if inner.configuring.is_some() {
                return Err("a session is already being configured".into());
            }
            if inner.scheduled.is_some() {
                return Err("a session is already scheduled".into());
            }
        }

        // WMDE: with `session_vt = next` the session gets a free VT of its own,
        // and the worker leaves the screen where it is (`switch: false`).
        let own_vt = match (self.session_vt, &self.term_mode) {
            (SessionVt::Next, TerminalMode::Terminal { .. }) => {
                let vt = Terminal::open("/dev/tty0")
                    .and_then(|term| term.vt_get_next())
                    .map_err(|e| format!("unable to get a VT for the session: {e}"))?;
                Some((format!("/dev/tty{vt}"), vt))
            }
            _ => None,
        };
        let term_mode = match &own_vt {
            Some((path, vt)) => TerminalMode::Terminal {
                path: path.clone(),
                vt: *vt,
                switch: false,
            },
            None => self.term_mode.clone(),
        };

        let mut session_set = SessionSet {
            session: Session::new_external()?,
            time: Instant::now(),
            own_vt,
        };
        session_set
            .session
            .initiate(
                &self.pam_service,
                SessionClass::User,
                &username,
                true,
                &term_mode,
                self.source_profile,
                &self.listener_path,
            )
            .await?;

        let mut session = Some(session_set);
        let mut inner = self.inner.write().await;
        std::mem::swap(&mut session, &mut inner.configuring);
        drop(inner);

        // If there was a session under configuration, cancel it.
        if let Some(mut s) = session {
            s.session.cancel().await?;
        }

        Ok(())
    }

    /// Cancel the session being configured.
    pub async fn cancel(&self) -> Result<(), Error> {
        let mut inner = self.inner.write().await;
        if let Some(mut s) = inner.configuring.take() {
            s.session.cancel().await?;
        }
        Ok(())
    }

    /// Retrieve a question from the session under configuration.
    pub async fn get_question(&self) -> Result<Option<(AuthMessageType, String)>, Error> {
        let mut inner = self.inner.write().await;
        match &mut inner.configuring {
            Some(s) => match s.session.get_state().await? {
                SessionState::Ready => Ok(None),
                SessionState::Question(style, string) => Ok(Some((
                    match style {
                        SessAuthMessageType::Visible => AuthMessageType::Visible,
                        SessAuthMessageType::Secret => AuthMessageType::Secret,
                        SessAuthMessageType::Info => AuthMessageType::Info,
                        SessAuthMessageType::Error => AuthMessageType::Error,
                    },
                    string,
                ))),
            },
            None => Err("no session under configuration".into()),
        }
    }

    /// Answer a question to the session under configuration.
    pub async fn post_response(&self, answer: Option<String>) -> Result<(), Error> {
        let mut inner = self.inner.write().await;
        match &mut inner.configuring {
            Some(s) => match s.session.get_state().await? {
                SessionState::Ready => Err("session has no pending questions".into()),
                _ => s.session.post_response(answer).await,
            },
            None => {
                if let Some(mut answer) = answer {
                    answer.scramble();
                }
                Err("no session under configuration".into())
            }
        }
    }

    /// Schedule the session under configuration with the provided arguments.
    pub async fn start(&self, cmd: Vec<String>, env: Vec<String>) -> Result<(), Error> {
        let mut session = self.inner.write().await.configuring.take();

        match &mut session {
            Some(s) => match s.session.get_state().await? {
                SessionState::Ready => {
                    // Send our arguments to the session.
                    s.session.send_args(cmd, env).await?;

                    // WMDE: a session on a VT of its own starts right away, next to
                    // the greeter. `complete_handover` takes it from there.
                    if let Some((path, vt)) = s.own_vt.clone() {
                        let child = s.session.start().await?;
                        let mut inner = self.inner.write().await;
                        inner.handover = Some(Handover {
                            child: SessionChildSet {
                                child,
                                time: Instant::now(),
                                is_greeter: false,
                            },
                            path,
                            vt,
                        });
                        return Ok(());
                    }

                    let mut inner = self.inner.write().await;
                    std::mem::swap(&mut session, &mut inner.scheduled);
                    drop(inner);

                    // If there was a scheduled session, cancel it.
                    if let Some(mut p) = session {
                        p.session.cancel().await?;
                    }

                    // We give the greeter 5 seconds to prove itself well-behaved before
                    // we lose patience and shoot it in the back repeatedly. This is all
                    // handled by our alarm handler.
                    alarm::set(5);

                    Ok(())
                }
                SessionState::Question(..) => Err("session is not ready".into()),
            },
            None => Err("no session active".into()),
        }
    }

    /// WMDE: finish a handover started by `start`. Waits for the session to
    /// take its VT over (logind puts the VT in graphics mode when the session's
    /// compositor takes control), switches to it, and only then asks the greeter
    /// to exit. The greeter's compositor is paused by the switch with its last
    /// frame still up, so the console never shows in between.
    pub async fn complete_handover(&self) -> Result<(), Error> {
        let (path, vt) = match &self.inner.read().await.handover {
            Some(h) => (h.path.clone(), h.vt),
            None => return Ok(()),
        };
        let term = Terminal::open(&path)?;

        let started = Instant::now();
        loop {
            if self.inner.read().await.handover.is_none() {
                // The session died before it was ready; `check_children` handles it.
                return Ok(());
            }
            if term.kd_is_graphics()? {
                break;
            }
            if started.elapsed() > HANDOVER_TIMEOUT {
                eprintln!("session on VT {vt} did not take its VT over, switching anyway");
                break;
            }
            sleep(HANDOVER_POLL).await;
        }

        term.vt_request_activate(vt)?;
        let switched = Instant::now();
        while term.vt_get_current()? != vt {
            if switched.elapsed() > HANDOVER_TIMEOUT {
                return Err(format!("switch to the session's VT {vt} did not happen").into());
            }
            sleep(HANDOVER_POLL).await;
        }

        let mut inner = self.inner.write().await;
        let Some(handover) = inner.handover.take() else {
            return Ok(());
        };
        if let Some(greeter) = inner.current.take() {
            greeter.child.term();
            inner.retiring = Some(greeter);
            // Shot down by `alarm` if it does not go on its own.
            alarm::set(5);
        }
        inner.current = Some(handover.child);
        Ok(())
    }

    /// Notify the Context of an alarm.
    pub async fn alarm(&self) -> Result<(), Error> {
        // Keep trying to terminate the greeter until it gives up.
        let mut inner = self.inner.write().await;

        // WMDE: a greeter that outlived its handover.
        if let Some(greeter) = &inner.retiring {
            greeter.child.kill();
        }

        if let Some(mut p) = inner.scheduled.take() {
            if let Some(g) = inner.current.take() {
                if p.time.elapsed() > Duration::from_secs(10) {
                    // We're out of patience.
                    g.child.kill();
                } else {
                    // Let's try to give it a gentle nudge.
                    g.child.term();
                }
                inner.current = Some(g);
                inner.scheduled = Some(p);
                alarm::set(1);
                return Ok(());
            }
            drop(inner);
            let s = match p.session.start().await {
                Ok(s) => s,
                Err(e) => return Err(format!("session start failed: {e}").into()),
            };
            let mut inner = self.inner.write().await;
            inner.current = Some(SessionChildSet {
                child: s,
                time: Instant::now(),
                is_greeter: false,
            });
        }

        Ok(())
    }

    /// Notify the Context that it needs to check its children for termination.
    /// This should be called on SIGCHLD.
    pub async fn check_children(&self) -> Result<(), Error> {
        loop {
            match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
                // No scheduled exits.
                Ok(WaitStatus::StillAlive) => break Ok(()),

                // We got an exit, see if it's something we need to clean up.
                Ok(WaitStatus::Exited(pid, ..)) | Ok(WaitStatus::Signaled(pid, ..)) => {
                    let mut inner = self.inner.write().await;

                    // WMDE: the greeter after a handover is done.
                    if inner
                        .retiring
                        .as_ref()
                        .is_some_and(|g| g.child.owns_pid(pid))
                    {
                        inner.retiring = None;
                        continue;
                    }

                    // WMDE: the session died before taking the screen over. The
                    // greeter has already let go of its UI, so replace it.
                    if inner
                        .handover
                        .as_ref()
                        .is_some_and(|h| h.child.child.owns_pid(pid))
                    {
                        eprintln!("session exited before taking its VT over");
                        inner.handover = None;
                        if let Some(greeter) = &inner.current {
                            greeter.child.term();
                            inner.restart_greeter = true;
                        }
                        continue;
                    }

                    let (was_greeter, sesion_length) = match &inner.current {
                        Some(s) if s.child.owns_pid(pid) => {
                            let res = (s.is_greeter, s.time.elapsed());
                            inner.current = None;
                            res
                        }
                        _ => continue,
                    };

                    match inner.scheduled.take() {
                        Some(mut scheduled) => {
                            // Our greeter finally bit the dust so we can
                            // start our scheduled session.
                            drop(inner);
                            let s = match scheduled.session.start().await {
                                Ok(s) => s,
                                Err(e) => return Err(format!("session start failed: {e}").into()),
                            };
                            let mut inner = self.inner.write().await;
                            inner.current = Some(SessionChildSet {
                                child: s,
                                time: Instant::now(),
                                is_greeter: false,
                            });
                        }
                        None => {
                            if was_greeter && !std::mem::take(&mut inner.restart_greeter) {
                                return Err("greeter exited without creating a session".into());
                            }
                            if sesion_length < Duration::from_secs(1) {
                                sleep(Duration::from_secs(1)).await;
                            }
                            inner.current = Some(SessionChildSet {
                                child: self.start_greeter().await?,
                                time: Instant::now(),
                                is_greeter: true,
                            });
                        }
                    }
                }

                // Useless status.
                Ok(_) => continue,

                // Interrupted.
                Err(nix::errno::Errno::EINTR) => continue,

                // We do not have any children right now.
                Err(nix::errno::Errno::ECHILD) => break Ok(()),

                // Uh, what?
                Err(e) => panic!("waitpid returned an unexpected error: {}", e),
            }
        }
    }

    /// Notify the Context that we want to terminate. This should be called on
    /// SIGTERM.
    pub async fn terminate(&self) -> Result<(), Error> {
        let mut inner = self.inner.write().await;
        if let Some(mut sess) = inner.configuring.take() {
            let _ = sess.session.cancel().await;
        }
        if let Some(mut sess) = inner.scheduled.take() {
            let _ = sess.session.cancel().await;
        }
        if let Some(sess) = inner.current.take() {
            sess.child.term();
        }
        if let Some(handover) = inner.handover.take() {
            handover.child.child.term();
        }
        if let Some(greeter) = inner.retiring.take() {
            greeter.child.term();
        }
        Ok(())
    }
}
