package com.redlynx.mt.snapmobile;

import com.redlynx.mt.snapmobile.actions.SMAction;
import com.redlynx.mt.snapmobile.actions.SMActionInit;
import com.redlynx.mt.snapmobile.actions.SMActionLogin;
import com.redlynx.mt.snapmobile.actions.SMActionLoginManual;
import com.redlynx.mt.snapmobile.actions.SMActionLogout;
import com.redlynx.mt.snapmobile.events.SMEvent;
import com.redlynx.mt.snapmobile.events.SMEventConnectedToGameroom;
import com.redlynx.mt.snapmobile.events.SMEventDataReceived;
import com.redlynx.mt.snapmobile.events.SMEventDisconnectedFromGameroom;
import com.redlynx.mt.snapmobile.events.SMEventDisconnectedFromNAF;
import com.redlynx.mt.snapmobile.events.SMEventJoinRequested;
import com.redlynx.mt.snapmobile.events.SMEventLoginCancelled;
import com.redlynx.mt.snapmobile.events.SMEventLoginCompleted;
import java.applet.Applet;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.LinkedList;
import java.util.Map;

/**
 * Reset Generation Revived replacement for RedLynx's SNAP Mobile glue.
 *
 * Same class, fields and methods as the original in rg-signed.jar (the engine-facing
 * NativeSNAPMobileInterface, the SMAction/SMEvent classes and the wrapper call into them),
 * but the Nokia SNAP/N-Gage Arena protocol is replaced by our WebSocket room relay
 * ({@link RelayConnection}, server in server/). Threading is unchanged: actions run on this
 * client's own thread, events are delivered to the engine from {@link #tick()} on the game thread.
 *
 * Login: no accounts. The player's nickname ({@link #nickname}, from revived.ini) is used
 * without showing the old Arena login screen.
 */
public class SNAPMobileClient implements Runnable {
    /** Relay endpoint and player nickname; set by the launcher before the client is created. */
    /** Relay address, or "auto": the relay of a game hosted on this network (rg.relay.LanDiscovery). */
    public static String relayUrl = "auto";
    /** How long Quick Match looks for a hosted game on the LAN. */
    private static final long AUTO_FIND_MS = 20000;
    public static String nickname = null;
    /** -Drg.netdebug=true prints every action, event, relay reply and game packet to stdout. */
    static final boolean DEBUG = Boolean.getBoolean("rg.netdebug");
    private static final long DEBUG_START = System.currentTimeMillis();

    static void debug(String s) {
        if (DEBUG) {
            long t = System.currentTimeMillis() - DEBUG_START;
            System.out.println("net: " + t / 1000 + "." + t / 100 % 10 + " " + s);
        }
    }

    /** "len: hex" of a game packet (the first 48 bytes). */
    static String hex(byte[] d) {
        StringBuilder b = new StringBuilder().append(d.length).append(':');
        for (int i = 0; i < d.length && i < 48; i++) {
            b.append(' ').append(Character.forDigit((d[i] >> 4) & 15, 16)).append(Character.forDigit(d[i] & 15, 16));
        }
        return d.length > 48 ? b.append(" ...").toString() : b.toString();
    }

    public static final int WEB_SERVICE_AUTH_FAILURE = 965;
    public static final int ERROR_LOGIN_NO_ERROR = 0;
    public static final int ERROR_LOGIN_AUTHENTICATION_ERROR = 1;
    public static final int ERROR_LOGIN_FAILED = 2;
    public static final int ERROR_CLOUD_NOT_CONNECT = 3;
    private static final int NETBUFFER_SIZE = 2048;
    private static final int MTU = 876;
    private static final int MAX_NUM_JOIN_RETRIES = 4;

    int loginErrorState = 0;
    private NativeSNAPMobileEventListener eventListener;
    private ServerCommBridge serverCommBridge = new ServerCommBridge();
    private final Object actionLock = new Object();
    private final Object eventLock = new Object();
    volatile String gameRoomID;
    volatile String gameRoomName;
    volatile boolean isInGameRoom;
    /** The room enterRoom is waiting for (see onRelayJoined). */
    private volatile String entering;
    private final LinkedList<SMEvent> eventsQueue = new LinkedList<SMEvent>();
    private final LinkedList<SMAction> actionsQueue = new LinkedList<SMAction>();
    boolean isRunning;
    int numJoinRetries;
    boolean logoutQueued;
    Thread mainThread;
    ByteBuffer netBuffer;
    private SNAPMobileSSOProvider clientSSOProvider;
    SNAPMobileLoginProvider loginProvider;
    private NativeSNAPMobileInterface nativeInterface;
    private volatile boolean threadRunning;
    public boolean credentialsOK;
    public boolean returnedFromLoginInternal = true;
    public boolean loginCancelled;
    private FriendListCache friendListCache = new FriendListCache();
    boolean initialSSOLoginFailed = false;
    private volatile RelayConnection relay;
    private String lastErrorMessage;

    public SNAPMobileClient(String host, String connectorsString, String lobbyName,
                            SNAPMobileLoginProvider loginProvider, int gameclass, String presence, Applet applet) {
        this.loginProvider = loginProvider;
        this.nativeInterface = new NativeSNAPMobileInterface(this);
        this.eventListener = new NativeSNAPMobileEventListener();
        this.gameRoomName = "";
        this.netBuffer = ByteBuffer.allocateDirect(NETBUFFER_SIZE);
        this.netBuffer.order(ByteOrder.nativeOrder());
        this.mainThread = new Thread(this, "SNAPMobileClient");
        this.mainThread.setDaemon(true);
        this.mainThread.setPriority(Thread.MAX_PRIORITY);
        this.nativeInterface.init();
    }

    public void initNative() {
        this.nativeInterface.initNative(this.netBuffer, NETBUFFER_SIZE, MTU);
    }

    public void uninit() {
        if (!this.isRunning) {
            return;
        }
        this.logoutInternal(true);
        this.isRunning = false;
        this.mainThread.interrupt();
        for (int i = 0; this.threadRunning && i < 200; i++) {
            try {
                Thread.sleep(10L);
            } catch (InterruptedException e) {
                break;
            }
        }
        this.nativeInterface.deinitNative();
        synchronized (this.actionLock) { this.actionsQueue.clear(); }
        synchronized (this.eventLock) { this.eventsQueue.clear(); }
    }

    public boolean initInternal() {
        if (!this.isRunning) {
            this.isRunning = true;
            this.mainThread.start();
            this.addAction(new SMActionInit(), true);
        }
        return this.isRunning;
    }

    // ------------------------------------------------------------------ rooms and data

    public int sendInternal(byte[] data) {
        RelayConnection r = this.relay;
        if (r == null || !r.sendPacket(data)) {
            if (this.isLoggedIn() && !this.logoutQueued) {
                this.handleError(0);
            }
        }
        return data.length;
    }

    /** create/join/move on the relay; true when we are now in the room. */
    private boolean enterRoom(String op, String name) {
        RelayConnection r = this.relay;
        if (r == null) {
            return false;
        }
        if (this.isInGameRoom && !name.equalsIgnoreCase(this.gameRoomName)) {
            this.leaveRoomInternal(this.gameRoomName);
        }
        Map<String, Object> reply;
        this.entering = name;
        try {
            reply = r.request("{\"t\":\"" + op + "\",\"room\":" + Json.quote(name) + "}", "joined");
        } finally {
            this.entering = null;
        }
        if (reply != null && "joined".equals(reply.get("t"))) {
            return true;    // onRelayJoined entered the room
        }
        SMLog.printf("Relay " + op + "(" + name + ") failed: " + (reply == null ? "timeout" : reply.get("code")) + "\n");
        this.isInGameRoom = false;
        this.gameRoomName = "";
        this.gameRoomID = null;
        return false;
    }

    public String createRoomInternal(String name) {
        return this.enterRoom("create", name) ? this.gameRoomID : null;
    }

    public void moveToRoom(String name) {
        this.enterRoom("move", name);
    }

    public boolean joinRoomInternal(String name) {
        SMLog.printf("Joining room " + name + "\n");
        return this.enterRoom("join", name);
    }

    public void leaveRoomInternal(String roomname) {
        SMLog.printf("Leaving room " + roomname + "\n");
        if (roomname != null && roomname.equalsIgnoreCase(this.gameRoomName)) {
            RelayConnection r = this.relay;
            if (r != null) {
                r.request("{\"t\":\"leave\"}", "left");
            }
            this.isInGameRoom = false;
            this.gameRoomName = "";
        }
        this.addEvent(new SMEventDisconnectedFromGameroom(roomname));
    }

    /**
     * Called on the WebSocket thread for the relay's "joined", before the packets right behind it.
     * The casino and game rooms greet a joiner at once; when the room was only entered by the
     * action thread after the reply, a greeting could arrive first and be dropped. Without the
     * casino's hello the engine never starts its keep-alive and drops the connection 20 s later
     * ("Connection..."), which happened now and then when coming back to the casino after a match.
     */
    void onRelayJoined() {
        String name = this.entering;
        if (name != null) {
            this.entering = null;
            this.gameRoomName = name;
            this.gameRoomID = name;
            this.isInGameRoom = true;
            this.addEvent(new SMEventConnectedToGameroom());
        }
    }

    /** Called on the WebSocket thread for every game packet from another room member. */
    void onRelayPacket(byte[] data) {
        if (DEBUG) {
            debug("recv " + hex(data));
        }
        if (this.isInGameRoom && data.length <= NETBUFFER_SIZE) {
            this.addEvent(new SMEventDataReceived(data, this.gameRoomName));
        }
    }

    /** Called on the WebSocket thread when the server connection drops. */
    void onRelayClosed(RelayConnection r, String why) {
        if (r != this.relay) {
            return;
        }
        SMLog.printf("Relay connection lost: " + why + "\n");
        if (this.isLoggedIn() && !this.logoutQueued) {
            this.handleError(0);
        }
    }

    // ------------------------------------------------------------------ login / logout

    public void logoutInternal(boolean setLoginCancelled) {
        SMLog.printf("Logging out\n");
        this.loginCancelled = setLoginCancelled;
        RelayConnection r = this.relay;
        this.relay = null;
        this.serverCommBridge.setConnected(false);
        if (r != null) {
            r.close();
        }
        this.isInGameRoom = false;
        this.gameRoomName = "";
        this.credentialsOK = false;
        this.addEvent(new SMEventLoginCancelled());
        this.logoutQueued = false;
    }

    public void loginInternal(boolean manual) {
        this.returnedFromLoginInternal = false;
        try {
            if (this.relay != null && this.relay.isOpen()) {
                this.credentialsOK = true;          // engine asked again; already connected
                this.addEvent(new SMEventLoginCompleted());
                return;
            }
            this.gameRoomName = "";
            this.isInGameRoom = false;
            String name = nickname;
            if (name == null || !SMArenaValidator.isValidArenaUsername(name)) {
                name = this.loginProvider != null ? this.loginProvider.getUsername() : null;
            }
            if (name == null || !SMArenaValidator.isValidArenaUsername(name)) {
                this.loginErrorState = ERROR_LOGIN_AUTHENTICATION_ERROR;
                return;
            }
            String url = relayUrl;
            if ("auto".equalsIgnoreCase(url)) {        // LAN: find the game hosted with F5
                url = rg.relay.LanDiscovery.find(AUTO_FIND_MS);
                if (url == null) {
                    SMLog.printf("No hosted game found on this network\n");
                    this.loginErrorState = ERROR_CLOUD_NOT_CONNECT;
                    return;
                }
            }
            RelayConnection r = RelayConnection.open(url, this);
            if (r == null) {
                this.loginErrorState = ERROR_CLOUD_NOT_CONNECT;
                return;
            }
            Map<String, Object> w = r.request("{\"t\":\"hello\",\"v\":1,\"name\":" + Json.quote(name) + "}", "welcome");
            if (w == null || !"welcome".equals(w.get("t"))) {
                SMLog.printf("Relay rejected login: " + (w == null ? "timeout" : w.get("code")) + "\n");
                r.close();
                this.loginErrorState = ERROR_LOGIN_FAILED;
                return;
            }
            if (this.loginCancelled || this.logoutQueued) {
                r.close();
                return;
            }
            SMLog.printf("Logged in to " + url + " as " + w.get("name") + "\n");
            this.relay = r;
            this.credentialsOK = true;
            this.serverCommBridge.setConnected(true);
            if (this.loginProvider != null) {
                this.loginProvider.loginCompleted();
            }
            this.addEvent(new SMEventLoginCompleted());
        } finally {
            this.returnedFromLoginInternal = true;
        }
    }

    public void showLoginScreenInternal() {
        this.loginErrorState = 0;
        this.credentialsOK = false;
        this.loginCancelled = false;
        if (nickname == null && this.loginProvider != null) {
            this.loginProvider.startLogin(new SNAPMobileLoginReceiver(this));
        }
        // With a nickname there is no login screen: the engine sees "logging in" and calls login().
    }

    // ------------------------------------------------------------------ friends (not supported yet)

    public void cacheBuddyList() {
        synchronized (this.friendListCache) {
            this.friendListCache.purge();
        }
    }

    public String getUserIDforName(String username) {
        return username;
    }

    // ------------------------------------------------------------------ queues

    public void tick() {
        SMEvent event;
        while ((event = this.pollEvent()) != null) {
            event.process(this);
        }
    }

    private SMEvent pollEvent() {
        synchronized (this.eventLock) {
            return this.eventsQueue.isEmpty() ? null : this.eventsQueue.removeFirst();
        }
    }

    public void run() {
        this.threadRunning = true;
        try {
            while (this.isRunning) {
                SMAction action = null;
                synchronized (this.actionLock) {
                    if (this.actionsQueue.isEmpty()) {
                        try {
                            this.actionLock.wait();
                        } catch (InterruptedException e) {
                            // re-check isRunning
                        }
                    }
                    if (!this.actionsQueue.isEmpty()) {
                        action = this.actionsQueue.removeFirst();
                    }
                }
                if (!this.isRunning) {
                    break;
                }
                if (action == null) {
                    continue;
                }
                try {
                    action.process(this);
                } catch (Exception e) {
                    SMLog.printStackTrace(e);
                    this.addEvent(new SMEventLoginCancelled());
                }
            }
        } finally {
            this.threadRunning = false;
        }
    }

    public void addEvent(SMEvent event) {
        debug("event " + event.getClass().getSimpleName());
        synchronized (this.eventLock) {
            this.eventsQueue.add(event);
        }
    }

    public void addAction(SMAction action, boolean allowNotLoggedIn) {
        synchronized (this.actionLock) {
            if (!(allowNotLoggedIn || !this.logoutQueued && this.isLoggedIn())) {
                debug("action dropped (not logged in) " + action.getClass().getSimpleName());
                return;
            }
            debug("action " + action.getClass().getSimpleName());
            if (action instanceof SMActionLogout) {
                this.logoutQueued = true;
            }
            if (action instanceof SMActionLoginManual || action instanceof SMActionLogin) {
                // A new attempt (NativeSNAPMobileInterface.login(), engine thread, before the engine
                // polls getLoginScreenStatus). The engine logs in with login() and never calls
                // startLoginScreen(), which is what reset these: an earlier "no host found" kept
                // reporting "cannot connect" at once, and an earlier logout reported "cancelled",
                // so the client could not get online again until it was restarted.
                this.loginErrorState = ERROR_LOGIN_NO_ERROR;
                this.loginCancelled = false;
            }
            this.actionsQueue.add(action);
            this.actionLock.notify();
        }
    }

    /**
     * The connection is gone: drop the pending actions and log out. A logout that is already
     * queued (e.g. the engine's own, from another thread) is kept: clearing it while logoutQueued
     * stayed set used to drop every later action and login for good.
     */
    private void handleError(int errorCode) {
        this.addEvent(new SMEventDisconnectedFromNAF(errorCode));
        synchronized (this.actionLock) {
            this.actionsQueue.removeIf(a -> !(a instanceof SMActionLogout));
            if (this.actionsQueue.isEmpty()) {
                debug("action SMActionLogout (connection lost)");
                this.logoutQueued = true;
                this.actionsQueue.add(new SMActionLogout(false));
                this.actionLock.notify();
            }
        }
    }

    public void retryJoinRoom(String name) {
        if (name != null && this.numJoinRetries < MAX_NUM_JOIN_RETRIES) {
            try {
                Thread.sleep(3000L);
            } catch (InterruptedException e) {
                // retry now
            }
            ++this.numJoinRetries;
            this.addEvent(new SMEventJoinRequested(name));
        } else {
            this.loginErrorState = ERROR_CLOUD_NOT_CONNECT;
            if (this.isLoggedIn() && !this.logoutQueued) {
                this.handleError(2);
            }
        }
    }

    public void processServerError(int error, String msg, int severity) {
        this.lastErrorMessage = msg;
    }

    // ------------------------------------------------------------------ accessors

    public boolean isRunning() {
        return this.isRunning;
    }

    public String getGameRoomID(String name) {
        return name;
    }

    public void setClientSSOProvider(SNAPMobileSSOProvider provider) {
        this.clientSSOProvider = provider;
    }

    String getLastErrorMessage() {
        return this.lastErrorMessage;
    }

    public SNAPMobileSSOProvider getClientSSOProvider() {
        return this.clientSSOProvider;
    }

    public NativeSNAPMobileEventListener getEventListener() {
        return this.eventListener;
    }

    public void setLoginCancelled(boolean cancelled) {
        this.loginCancelled = cancelled;
    }

    public ByteBuffer getNetBuffer() {
        return this.netBuffer;
    }

    public FriendListCache getFriendListCache() {
        return this.friendListCache;
    }

    public boolean isLoggedIn() {
        return this.serverCommBridge.isLoggedIn();
    }

    public ServerCommBridge getServerCommBridge() {
        return this.serverCommBridge;
    }

    public boolean isLoginInProgress() {
        return !this.returnedFromLoginInternal;
    }

    public String getSNAPHost() {
        return relayUrl;
    }
}
