// Superba Windows e2e desktop agent (compiled by agent.ps1 with Add-Type, so C# 5 only:
// no string interpolation, no expression-bodied members, no ?. operators).
//
// Runs in the logged-on user's interactive desktop session (started by a one-off
// scheduled task with an Interactive principal and the *limited* run level, so its
// children get the same unelevated token a user's Start-menu click gives them). The
// host reaches it through an SSH local forward to 127.0.0.1:<port>; every request
// carries a random token. One JSON request line per TCP connection, one JSON answer
// line back. Requests are never logged with their arguments (they can carry a key
// that is being typed), only the command name, duration and outcome.
//
// Commands: ping, windows, window, uia.tree, uia.find, uia.act, click, scroll, keys, key,
// shot, exec, start, kill, procs, env.user, cdp.list, cdp.call, quit.

using System;
using System.Collections;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Management;
using System.Net;
using System.Net.Sockets;
using System.Net.WebSockets;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;
using System.Web.Script.Serialization;
using System.Windows.Automation;
using Microsoft.Win32;

public static class WtAgent
{
    // ------------------------------------------------------------------ Win32
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Sequential)]
    struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Sequential)]
    struct HARDWAREINPUT { public uint uMsg; public ushort wParamL; public ushort wParamH; }
    [StructLayout(LayoutKind.Explicit)]
    struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; [FieldOffset(0)] public HARDWAREINPUT hi; }
    [StructLayout(LayoutKind.Sequential)]
    struct INPUT { public uint type; public InputUnion u; }

    delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint flags);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool AttachThreadInput(uint from, uint to, bool attach);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] static extern bool IsZoomed(IntPtr h);
    [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr h, uint cmd);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr SendMessageTimeout(IntPtr h, uint msg, IntPtr w, string l, uint flags, uint timeout, out IntPtr result);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
    [DllImport("kernel32.dll")] static extern uint WTSGetActiveConsoleSessionId();
    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("advapi32.dll", SetLastError = true)] static extern bool OpenProcessToken(IntPtr h, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true)] static extern bool GetTokenInformation(IntPtr token, int cls, IntPtr info, int len, out int ret);
    [DllImport("advapi32.dll")] static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
    [DllImport("advapi32.dll")] static extern IntPtr GetSidSubAuthority(IntPtr sid, uint n);

    // ------------------------------------------------------------------ state
    static readonly JavaScriptSerializer Json = new JavaScriptSerializer();
    static string token = "";
    static string logPath = null;
    static readonly object logLock = new object();
    static volatile bool quit = false;
    static HashSet<string> startUserEnv = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

    public static void Serve(int port, string tok, string log)
    {
        Json.MaxJsonLength = int.MaxValue;
        Json.RecursionLimit = 512;
        token = tok;
        logPath = log;
        SetProcessDPIAware();
        foreach (string n in UserEnvNames()) startUserEnv.Add(n);
        TcpListener l = new TcpListener(IPAddress.Loopback, port);
        l.Start();
        Log("listening on 127.0.0.1:" + port + " pid " + Process.GetCurrentProcess().Id);
        while (!quit)
        {
            if (!l.Pending()) { Thread.Sleep(50); continue; }
            TcpClient c = l.AcceptTcpClient();
            Thread t = new Thread(delegate () { Client(c); });
            t.IsBackground = true;
            t.SetApartmentState(ApartmentState.MTA);
            t.Start();
        }
        Thread.Sleep(300);
        l.Stop();
        Log("stopped");
    }

    static void Log(string s)
    {
        if (logPath == null) return;
        lock (logLock)
        {
            try { File.AppendAllText(logPath, DateTime.UtcNow.ToString("HH:mm:ss.fff") + " " + s + "\r\n"); } catch { }
        }
    }

    static void Client(TcpClient c)
    {
        string cmd = "?";
        Stopwatch sw = Stopwatch.StartNew();
        Dictionary<string, object> resp = new Dictionary<string, object>();
        try
        {
            NetworkStream s = c.GetStream();
            StreamReader r = new StreamReader(s, new UTF8Encoding(false));
            StreamWriter w = new StreamWriter(s, new UTF8Encoding(false));
            w.AutoFlush = true;
            string line = r.ReadLine();
            if (line == null) return;
            try
            {
                Dictionary<string, object> req = Json.DeserializeObject(line) as Dictionary<string, object>;
                if (req == null) throw new ArgumentException("request is not a JSON object");
                if (S(req, "token", "") != token) throw new UnauthorizedAccessException("bad token");
                cmd = S(req, "cmd", "");
                Dictionary<string, object> a = req.ContainsKey("args") ? req["args"] as Dictionary<string, object> : null;
                if (a == null) a = new Dictionary<string, object>();
                resp["result"] = Dispatch(cmd, a);
                resp["ok"] = true;
            }
            catch (Exception e)
            {
                Exception x = e;
                while (x.InnerException != null && (x is System.Reflection.TargetInvocationException || x is AggregateException)) x = x.InnerException;
                resp["ok"] = false;
                resp["error"] = x.GetType().Name + ": " + x.Message;
            }
            resp["ms"] = sw.ElapsedMilliseconds;
            w.WriteLine(Json.Serialize(resp));
            Log(cmd + " " + sw.ElapsedMilliseconds + "ms " + ((bool)resp["ok"] ? "ok" : "ERR " + resp["error"]));
        }
        catch (Exception e) { Log("client failure " + cmd + ": " + e.Message); }
        finally { try { c.Close(); } catch { } }
    }

    // ------------------------------------------------------------------ args
    static string S(Dictionary<string, object> a, string k, string d)
    {
        object v;
        if (a.TryGetValue(k, out v) && v != null) return Convert.ToString(v, System.Globalization.CultureInfo.InvariantCulture);
        return d;
    }
    static int I(Dictionary<string, object> a, string k, int d)
    {
        object v;
        if (a.TryGetValue(k, out v) && v != null) return Convert.ToInt32(v, System.Globalization.CultureInfo.InvariantCulture);
        return d;
    }
    static long L(Dictionary<string, object> a, string k, long d)
    {
        object v;
        if (a.TryGetValue(k, out v) && v != null) return Convert.ToInt64(v, System.Globalization.CultureInfo.InvariantCulture);
        return d;
    }
    static bool B(Dictionary<string, object> a, string k, bool d)
    {
        object v;
        if (a.TryGetValue(k, out v) && v != null) return Convert.ToBoolean(v);
        return d;
    }
    static Dictionary<string, object> D(Dictionary<string, object> a, string k)
    {
        object v;
        if (a.TryGetValue(k, out v)) return v as Dictionary<string, object>;
        return null;
    }
    static List<string> SL(Dictionary<string, object> a, string k)
    {
        List<string> o = new List<string>();
        object v;
        if (a.TryGetValue(k, out v) && v is object[])
            foreach (object x in (object[])v) o.Add(Convert.ToString(x));
        return o;
    }

    // ------------------------------------------------------------------ dispatch
    static object Dispatch(string cmd, Dictionary<string, object> a)
    {
        switch (cmd)
        {
            case "ping": return Ping();
            case "windows": return Windows(a);
            case "window": return WindowOp(a);
            case "uia.tree": return UiaTree(a);
            case "uia.snapshot": return UiaSnapshot(a);
            case "uia.find": return UiaFind(a);
            case "uia.act": return UiaAct(a);
            case "click": return Click(a);
            case "hover": return Hover(a);
            case "scroll": return Scroll(a);
            case "keys": TypeText(S(a, "text", "")); return true;
            case "key": return KeyCombo(S(a, "combo", ""), I(a, "repeat", 1));
            case "shot": return Shot(a);
            case "exec": return Exec(a);
            case "start": return Start(a);
            case "kill": return Kill(a);
            case "procs": return Procs(a);
            case "proc.integrity": return Integrity(I(a, "pid", 0));
            case "env.user": return UserEnv();
            case "file.read": return FileRead(a);
            case "env.broadcast": return Broadcast();
            case "cdp.list": return CdpList(I(a, "port", 0));
            case "cdp.call": return CdpCall(a);
            case "quit": quit = true; return true;
        }
        throw new ArgumentException("unknown command " + cmd);
    }

    static object Ping()
    {
        Dictionary<string, object> o = new Dictionary<string, object>();
        Process p = Process.GetCurrentProcess();
        WindowsIdentity id = WindowsIdentity.GetCurrent();
        o["pid"] = p.Id;
        o["session"] = p.SessionId;
        o["console_session"] = (long)WTSGetActiveConsoleSessionId();
        o["user"] = id.Name;
        o["admin_token"] = new WindowsPrincipal(id).IsInRole(WindowsBuiltInRole.Administrator);
        o["screen"] = new int[] { GetSystemMetrics(0), GetSystemMetrics(1) };
        o["clr"] = Environment.Version.ToString();
        return o;
    }

    // ------------------------------------------------------------------ windows
    static Dictionary<string, object> WinInfo(IntPtr h)
    {
        Dictionary<string, object> o = new Dictionary<string, object>();
        StringBuilder sb = new StringBuilder(512);
        GetWindowText(h, sb, sb.Capacity);
        StringBuilder cb = new StringBuilder(256);
        GetClassName(h, cb, cb.Capacity);
        uint pid;
        GetWindowThreadProcessId(h, out pid);
        RECT r = Bounds(h);
        o["hwnd"] = h.ToInt64();
        o["title"] = sb.ToString();
        o["class"] = cb.ToString();
        o["pid"] = (long)pid;
        string pname = "";
        try { pname = Process.GetProcessById((int)pid).ProcessName; } catch { }
        o["process"] = pname;
        o["rect"] = new int[] { r.Left, r.Top, r.Right - r.Left, r.Bottom - r.Top };
        o["visible"] = IsWindowVisible(h);
        o["minimized"] = IsIconic(h);
        o["maximized"] = IsZoomed(h);
        o["foreground"] = GetForegroundWindow() == h;
        o["owner"] = GetWindow(h, 4).ToInt64();
        return o;
    }

    static RECT Bounds(IntPtr h)
    {
        RECT r;
        // DWMWA_EXTENDED_FRAME_BOUNDS: the visible frame without the invisible resize borders.
        if (DwmGetWindowAttribute(h, 9, out r, Marshal.SizeOf(typeof(RECT))) != 0) GetWindowRect(h, out r);
        return r;
    }

    static List<Dictionary<string, object>> TopWindows(Dictionary<string, object> a)
    {
        long pid = L(a, "pid", 0);
        string procRe = S(a, "process_re", null);
        string titleRe = S(a, "title_re", null);
        bool all = B(a, "all", false);
        List<Dictionary<string, object>> o = new List<Dictionary<string, object>>();
        EnumWindows(delegate (IntPtr h, IntPtr l)
        {
            if (!all && !IsWindowVisible(h)) return true;
            Dictionary<string, object> w = WinInfo(h);
            if (pid != 0 && (long)w["pid"] != pid) return true;
            if (procRe != null && !Regex.IsMatch((string)w["process"], procRe, RegexOptions.IgnoreCase)) return true;
            if (titleRe != null && !Regex.IsMatch((string)w["title"], titleRe)) return true;
            int[] rc = (int[])w["rect"];
            if (!all && (rc[2] <= 1 || rc[3] <= 1)) return true;
            o.Add(w);
            return true;
        }, IntPtr.Zero);
        return o;
    }

    static object Windows(Dictionary<string, object> a) { return TopWindows(a); }

    static IntPtr ResolveWindow(Dictionary<string, object> a, bool required)
    {
        long hwnd = L(a, "hwnd", 0);
        if (hwnd != 0) return new IntPtr(hwnd);
        if (L(a, "pid", 0) == 0 && S(a, "process_re", null) == null && S(a, "title_re", null) == null)
        {
            if (required) throw new ArgumentException("no window selector (hwnd, pid, process_re, title_re)");
            return IntPtr.Zero;
        }
        int waitMs = I(a, "win_timeout_ms", 0);
        Stopwatch sw = Stopwatch.StartNew();
        while (true)
        {
            List<Dictionary<string, object>> ws = TopWindows(a);
            // Prefer an unowned window with a title (the main window), then the largest.
            Dictionary<string, object> best = null;
            foreach (Dictionary<string, object> w in ws)
            {
                if (best == null) { best = w; continue; }
                bool wMain = (long)w["owner"] == 0 && ((string)w["title"]).Length > 0;
                bool bMain = (long)best["owner"] == 0 && ((string)best["title"]).Length > 0;
                int[] wr = (int[])w["rect"], br = (int[])best["rect"];
                if ((wMain && !bMain) || (wMain == bMain && wr[2] * wr[3] > br[2] * br[3])) best = w;
            }
            if (best != null) return new IntPtr((long)best["hwnd"]);
            if (sw.ElapsedMilliseconds >= waitMs)
            {
                if (required) throw new InvalidOperationException("window not found");
                return IntPtr.Zero;
            }
            Thread.Sleep(250);
        }
    }

    static object WindowOp(Dictionary<string, object> a)
    {
        IntPtr h = ResolveWindow(a, true);
        if (B(a, "restore", false) && (IsIconic(h) || IsZoomed(h))) ShowWindow(h, 9);
        if (B(a, "minimize", false)) ShowWindow(h, 6);
        if (B(a, "maximize", false)) ShowWindow(h, 3);
        if (a.ContainsKey("w") || a.ContainsKey("x"))
        {
            RECT r = Bounds(h);
            RECT raw; GetWindowRect(h, out raw);
            // SetWindowPos takes the raw rect (with invisible borders); keep the visible
            // frame at the requested position/size.
            int bl = r.Left - raw.Left, bt = r.Top - raw.Top, br = raw.Right - r.Right, bb = raw.Bottom - r.Bottom;
            int x = I(a, "x", r.Left) - bl, y = I(a, "y", r.Top) - bt;
            int w = I(a, "w", r.Right - r.Left) + bl + br, hh = I(a, "h", r.Bottom - r.Top) + bt + bb;
            SetWindowPos(h, IntPtr.Zero, x, y, w, hh, 0x0004 /*NOZORDER*/ | 0x0010 /*NOACTIVATE*/);
        }
        if (B(a, "foreground", false)) Foreground(h);
        if (B(a, "close", false)) PostMessage(h, 0x0010 /*WM_CLOSE*/, IntPtr.Zero, IntPtr.Zero);
        Thread.Sleep(I(a, "settle_ms", 150));
        return IsWindow(h) ? (object)WinInfo(h) : null;
    }

    static void Foreground(IntPtr h)
    {
        if (IsIconic(h)) ShowWindow(h, 9);
        if (GetForegroundWindow() == h) return;
        BringWindowToTop(h);
        SetForegroundWindow(h);
        if (GetForegroundWindow() != h)
        {
            // The agent's worker thread may not own the foreground input queue.
            // Share it only for activation, and always detach before doing UIA work.
            uint pid, current = GetCurrentThreadId();
            uint foreground = GetWindowThreadProcessId(GetForegroundWindow(), out pid);
            bool attached = foreground != 0 && foreground != current
                && AttachThreadInput(current, foreground, true);
            try { BringWindowToTop(h); SetForegroundWindow(h); }
            finally { if (attached) AttachThreadInput(current, foreground, false); }
        }
        if (GetForegroundWindow() != h)
        {
            // Keep Alt down while activating; releasing it before activation can
            // open the other application's menu and prevent the foreground change.
            KeyEvent(0x12, false, false);
            try { SetForegroundWindow(h); }
            finally { KeyEvent(0x12, true, false); }
        }
        for (int i = 0; i < 10 && GetForegroundWindow() != h; i++) { Thread.Sleep(50); SetForegroundWindow(h); }
        if (GetForegroundWindow() != h)
            throw new InvalidOperationException("could not activate the target window; input/capture cancelled");
    }

    // ------------------------------------------------------------------ UI Automation
    static AutomationElement RootOf(Dictionary<string, object> a)
    {
        IntPtr h = ResolveWindow(a, true);
        return AutomationElement.FromHandle(h);
    }

    static string TypeName(AutomationElement e)
    {
        try { return e.Current.ControlType.ProgrammaticName.Replace("ControlType.", ""); } catch { return "?"; }
    }

    static Dictionary<string, object> ElInfo(AutomationElement e, bool patterns)
    {
        Dictionary<string, object> o = new Dictionary<string, object>();
        AutomationElement.AutomationElementInformation c = e.Current;
        o["name"] = c.Name;
        o["type"] = TypeName(e);
        o["aid"] = c.AutomationId;
        o["class"] = c.ClassName;
        System.Windows.Rect r = c.BoundingRectangle;
        if (r.IsEmpty) o["rect"] = null;
        else o["rect"] = new int[] { (int)r.X, (int)r.Y, (int)r.Width, (int)r.Height };
        o["enabled"] = c.IsEnabled;
        o["offscreen"] = c.IsOffscreen;
        o["focus"] = c.HasKeyboardFocus;
        o["focusable"] = c.IsKeyboardFocusable;
        if (!string.IsNullOrEmpty(c.HelpText)) o["help"] = c.HelpText;
        if (!string.IsNullOrEmpty(c.ItemStatus)) o["status"] = c.ItemStatus;
        if (patterns)
        {
            List<string> ps = new List<string>();
            object p;
            if (e.TryGetCurrentPattern(InvokePattern.Pattern, out p)) ps.Add("invoke");
            if (e.TryGetCurrentPattern(TogglePattern.Pattern, out p))
            {
                ps.Add("toggle");
                o["toggle"] = ((TogglePattern)p).Current.ToggleState.ToString();
            }
            if (e.TryGetCurrentPattern(ValuePattern.Pattern, out p))
            {
                ps.Add("value");
                try { o["value"] = ((ValuePattern)p).Current.Value; o["readonly"] = ((ValuePattern)p).Current.IsReadOnly; } catch { }
            }
            if (e.TryGetCurrentPattern(SelectionItemPattern.Pattern, out p))
            {
                ps.Add("select");
                try { o["selected"] = ((SelectionItemPattern)p).Current.IsSelected; } catch { }
            }
            if (e.TryGetCurrentPattern(ExpandCollapsePattern.Pattern, out p))
            {
                ps.Add("expand");
                try { o["expanded"] = ((ExpandCollapsePattern)p).Current.ExpandCollapseState.ToString(); } catch { }
            }
            if (e.TryGetCurrentPattern(RangeValuePattern.Pattern, out p))
            {
                ps.Add("range");
                try { o["range"] = ((RangeValuePattern)p).Current.Value; } catch { }
            }
            if (e.TryGetCurrentPattern(ScrollItemPattern.Pattern, out p)) ps.Add("scrollitem");
            if (e.TryGetCurrentPattern(ScrollPattern.Pattern, out p)) ps.Add("scroll");
            o["patterns"] = ps;
        }
        return o;
    }

    // Depth-first walk of the control view; the callback returns false to stop.
    delegate bool Visit(AutomationElement e, int depth, string path);
    static void Walk(AutomationElement root, int maxDepth, Visit v, bool raw)
    {
        TreeWalker tw = raw ? TreeWalker.RawViewWalker : TreeWalker.ControlViewWalker;
        Stack<object[]> st = new Stack<object[]>();
        st.Push(new object[] { root, 0, "0" });
        while (st.Count > 0)
        {
            object[] it = st.Pop();
            AutomationElement e = (AutomationElement)it[0];
            int d = (int)it[1];
            string path = (string)it[2];
            bool go;
            try { go = v(e, d, path); } catch (ElementNotAvailableException) { continue; }
            if (!go) return;
            if (d >= maxDepth) continue;
            List<AutomationElement> kids = new List<AutomationElement>();
            try
            {
                AutomationElement k = tw.GetFirstChild(e);
                while (k != null) { kids.Add(k); k = tw.GetNextSibling(k); }
            }
            catch (ElementNotAvailableException) { }
            for (int i = kids.Count - 1; i >= 0; i--) st.Push(new object[] { kids[i], d + 1, path + "." + i });
        }
    }

    static object UiaTree(Dictionary<string, object> a)
    {
        AutomationElement root = RootOf(a);
        int maxDepth = I(a, "depth", 80), max = I(a, "max", 4000);
        bool pats = B(a, "patterns", true);
        List<Dictionary<string, object>> o = new List<Dictionary<string, object>>();
        Walk(root, maxDepth, delegate (AutomationElement e, int d, string path)
        {
            Dictionary<string, object> i = ElInfo(e, pats);
            i["depth"] = d;
            i["path"] = path;
            o.Add(i);
            return o.Count < max;
        }, B(a, "raw", false));
        return o;
    }

    // One provider cache request, then local reads. A live property-by-property
    // tree walk can take longer than a short migration while its UI is changing.
    static object UiaSnapshot(Dictionary<string, object> a)
    {
        CacheRequest cache = new CacheRequest();
        cache.TreeScope = TreeScope.Subtree;
        cache.TreeFilter = Automation.RawViewCondition;
        cache.AutomationElementMode = AutomationElementMode.None;
        cache.Add(AutomationElement.NameProperty);
        cache.Add(AutomationElement.ControlTypeProperty);
        cache.Add(AutomationElement.BoundingRectangleProperty);
        cache.Add(AutomationElement.IsEnabledProperty);
        AutomationElement root = RootOf(a).GetUpdatedCache(cache);
        List<Dictionary<string, object>> result = new List<Dictionary<string, object>>();
        Stack<AutomationElement> pending = new Stack<AutomationElement>();
        pending.Push(root);
        while (pending.Count > 0)
        {
            AutomationElement element = pending.Pop();
            AutomationElement.AutomationElementInformation c = element.Cached;
            System.Windows.Rect r = c.BoundingRectangle;
            Dictionary<string, object> row = new Dictionary<string, object>();
            row["name"] = c.Name;
            row["type"] = c.ControlType.ProgrammaticName.Replace("ControlType.", "");
            row["rect"] = r.IsEmpty ? null : new int[] { (int)r.X, (int)r.Y, (int)r.Width, (int)r.Height };
            row["enabled"] = c.IsEnabled;
            result.Add(row);
            AutomationElementCollection children = element.CachedChildren;
            if (children != null)
                for (int i = children.Count - 1; i >= 0; i--) pending.Push(children[i]);
        }
        return result;
    }

    class Sel
    {
        public string Name, NameRe, Contains, Type, Aid, HelpRe;
        public bool? Enabled;
        public bool Visible = true;
        public int Index;
        public bool Match(AutomationElement e)
        {
            AutomationElement.AutomationElementInformation c = e.Current;
            string n = c.Name ?? "";
            if (Name != null && n != Name) return false;
            if (NameRe != null && !Regex.IsMatch(n, NameRe)) return false;
            if (Contains != null && n.IndexOf(Contains, StringComparison.OrdinalIgnoreCase) < 0) return false;
            if (Type != null && !string.Equals(TypeName(e), Type, StringComparison.OrdinalIgnoreCase)) return false;
            if (Aid != null && c.AutomationId != Aid) return false;
            if (HelpRe != null && !Regex.IsMatch(c.HelpText ?? "", HelpRe)) return false;
            if (Enabled.HasValue && c.IsEnabled != Enabled.Value) return false;
            if (Visible)
            {
                System.Windows.Rect r = c.BoundingRectangle;
                if (c.IsOffscreen || r.IsEmpty || r.Width < 1 || r.Height < 1) return false;
            }
            return true;
        }
    }

    static Sel ParseSel(Dictionary<string, object> a)
    {
        Sel s = new Sel();
        s.Name = S(a, "name", null);
        s.NameRe = S(a, "name_re", null);
        s.Contains = S(a, "contains", null);
        s.Type = S(a, "type", null);
        s.Aid = S(a, "aid", null);
        s.HelpRe = S(a, "help_re", null);
        if (a.ContainsKey("enabled") && a["enabled"] != null) s.Enabled = B(a, "enabled", true);
        s.Visible = B(a, "visible", true);
        s.Index = I(a, "index", 0);
        return s;
    }

    static List<AutomationElement> FindAll(AutomationElement root, Sel s, int maxDepth, bool raw)
    {
        List<AutomationElement> o = new List<AutomationElement>();
        Walk(root, maxDepth, delegate (AutomationElement e, int d, string path)
        {
            if (s.Match(e)) o.Add(e);
            return o.Count < 500;
        }, raw);
        return o;
    }

    // Finds within the window, optionally inside the first element matching "within"
    // (a nested selector), waiting up to timeout_ms for the index-th match.
    static List<AutomationElement> FindWait(Dictionary<string, object> a)
    {
        int timeout = I(a, "timeout_ms", 0);
        Sel s = ParseSel(a);
        Dictionary<string, object> within = D(a, "within");
        bool raw = B(a, "raw", false);
        Stopwatch sw = Stopwatch.StartNew();
        while (true)
        {
            List<AutomationElement> found = new List<AutomationElement>();
            try
            {
                AutomationElement root = RootOf(a);
                if (within != null)
                {
                    List<AutomationElement> scopes = FindAll(root, ParseSel(within), 80, raw);
                    int si = I(within, "index", 0);
                    if (scopes.Count > si) found = FindAll(scopes[si], s, 80, raw);
                }
                else found = FindAll(root, s, 80, raw);
            }
            catch (ElementNotAvailableException) { }
            catch (InvalidOperationException) { if (timeout == 0) throw; }
            if (found.Count > s.Index || sw.ElapsedMilliseconds >= timeout) return found;
            Thread.Sleep(250);
        }
    }

    static object UiaFind(Dictionary<string, object> a)
    {
        List<Dictionary<string, object>> o = new List<Dictionary<string, object>>();
        foreach (AutomationElement e in FindWait(a))
        {
            try { o.Add(ElInfo(e, true)); } catch (ElementNotAvailableException) { }
        }
        return o;
    }

    static object UiaAct(Dictionary<string, object> a)
    {
        string action = S(a, "action", "invoke");
        Sel s = ParseSel(a);
        List<AutomationElement> found = FindWait(a);
        if (found.Count <= s.Index)
            throw new InvalidOperationException("element not found (" + found.Count + " matches)");
        AutomationElement e = found[s.Index];
        Dictionary<string, object> info = ElInfo(e, true);
        string how = action;
        object p;
        IntPtr win = ResolveWindow(a, false);
        // A native popup menu is not itself a foreground window. Its UIA
        // InvokePattern can run without activation; mouse fallbacks still
        // validate the foreground window in ClickElement.
        if (win != IntPtr.Zero && (action != "invoke" || B(a, "foreground", true))) Foreground(win);
        switch (action)
        {
            case "invoke":
                if (e.TryGetCurrentPattern(InvokePattern.Pattern, out p)) { ((InvokePattern)p).Invoke(); how = "InvokePattern"; }
                else if (e.TryGetCurrentPattern(TogglePattern.Pattern, out p)) { ((TogglePattern)p).Toggle(); how = "TogglePattern"; }
                else if (e.TryGetCurrentPattern(SelectionItemPattern.Pattern, out p)) { ((SelectionItemPattern)p).Select(); how = "SelectionItemPattern"; }
                else if (e.TryGetCurrentPattern(ExpandCollapsePattern.Pattern, out p)) { ((ExpandCollapsePattern)p).Expand(); how = "ExpandCollapsePattern"; }
                else { ClickElement(e, win, false, false); how = "mouse"; }
                break;
            case "click": ClickElement(e, win, false, false); break;
            case "double_click": ClickElement(e, win, false, true); break;
            case "right_click": ClickElement(e, win, true, false); break;
            case "toggle": ((TogglePattern)e.GetCurrentPattern(TogglePattern.Pattern)).Toggle(); break;
            case "select": ((SelectionItemPattern)e.GetCurrentPattern(SelectionItemPattern.Pattern)).Select(); break;
            case "expand": ((ExpandCollapsePattern)e.GetCurrentPattern(ExpandCollapsePattern.Pattern)).Expand(); break;
            case "collapse": ((ExpandCollapsePattern)e.GetCurrentPattern(ExpandCollapsePattern.Pattern)).Collapse(); break;
            case "scroll_into_view": ((ScrollItemPattern)e.GetCurrentPattern(ScrollItemPattern.Pattern)).ScrollIntoView(); break;
            case "focus":
                if (win != IntPtr.Zero) Foreground(win);
                e.SetFocus();
                break;
            case "set_value":
                ((ValuePattern)e.GetCurrentPattern(ValuePattern.Pattern)).SetValue(S(a, "value", ""));
                how = "ValuePattern";
                break;
            case "type":
                // Click into the field, select everything, then type like a user.
                ClickElement(e, win, false, false);
                Thread.Sleep(120);
                if (B(a, "clear", true)) { KeyCombo("ctrl+a", 1); KeyCombo("delete", 1); }
                TypeText(S(a, "value", ""));
                how = "keyboard";
                break;
            default: throw new ArgumentException("unknown action " + action);
        }
        Thread.Sleep(I(a, "settle_ms", 200));
        info["how"] = how;
        info["matches"] = found.Count;
        return info;
    }

    static void ClickElement(AutomationElement e, IntPtr win, bool right, bool dbl)
    {
        System.Windows.Rect r = e.Current.BoundingRectangle;
        if (r.IsEmpty) throw new InvalidOperationException("element has no bounding rectangle");
        if (win != IntPtr.Zero) Foreground(win);
        MouseClick((int)(r.X + r.Width / 2), (int)(r.Y + r.Height / 2), right, dbl);
    }

    // ------------------------------------------------------------------ input
    static void SendOne(INPUT i) { SendInput(1, new INPUT[] { i }, Marshal.SizeOf(typeof(INPUT))); }

    static void MouseClick(int x, int y, bool right, bool dbl)
    {
        SetCursorPos(x, y);
        Thread.Sleep(60);
        uint down = right ? 0x0008u : 0x0002u, up = right ? 0x0010u : 0x0004u;
        for (int n = 0; n < (dbl ? 2 : 1); n++)
        {
            INPUT i = new INPUT(); i.type = 0; i.u.mi.dwFlags = down; SendOne(i);
            Thread.Sleep(30);
            i.u.mi.dwFlags = up; SendOne(i);
            Thread.Sleep(dbl ? 60 : 30);
        }
    }

    static object Click(Dictionary<string, object> a)
    {
        IntPtr win = ResolveWindow(a, false);
        int x = I(a, "x", 0), y = I(a, "y", 0);
        if (win != IntPtr.Zero)
        {
            Foreground(win);
            if (B(a, "relative", false)) { RECT r = Bounds(win); x += r.Left; y += r.Top; }
        }
        MouseClick(x, y, S(a, "button", "left") == "right", B(a, "double", false));
        Thread.Sleep(I(a, "settle_ms", 150));
        return new int[] { x, y };
    }

    // Moves the pointer onto a point (tooltips), relative to the window when asked.
    static object Hover(Dictionary<string, object> a)
    {
        IntPtr win = ResolveWindow(a, false);
        int x = I(a, "x", 0), y = I(a, "y", 0);
        if (win != IntPtr.Zero)
        {
            Foreground(win);
            if (B(a, "relative", false)) { RECT r = Bounds(win); x += r.Left; y += r.Top; }
        }
        SetCursorPos(x - 3, y - 3);
        Thread.Sleep(60);
        SetCursorPos(x, y);
        Thread.Sleep(I(a, "settle_ms", 1200));
        return new int[] { x, y };
    }

    static object Scroll(Dictionary<string, object> a)
    {
        IntPtr win = ResolveWindow(a, false);
        int x = I(a, "x", 0), y = I(a, "y", 0);
        if (win != IntPtr.Zero)
        {
            Foreground(win);
            if (B(a, "relative", false)) { RECT r = Bounds(win); x += r.Left; y += r.Top; }
        }
        SetCursorPos(x, y);
        Thread.Sleep(50);
        int notches = I(a, "notches", -3);
        for (int n = 0; n < Math.Abs(notches); n++)
        {
            INPUT i = new INPUT(); i.type = 0; i.u.mi.dwFlags = 0x0800; i.u.mi.mouseData = (uint)(notches < 0 ? -120 : 120);
            SendOne(i);
            Thread.Sleep(40);
        }
        Thread.Sleep(I(a, "settle_ms", 150));
        return true;
    }

    static void KeyEvent(ushort vk, bool up, bool ext)
    {
        INPUT i = new INPUT();
        i.type = 1;
        i.u.ki.wVk = vk;
        i.u.ki.dwFlags = (up ? 0x0002u : 0u) | (ext ? 0x0001u : 0u);
        SendOne(i);
    }

    static void TypeText(string text)
    {
        foreach (char ch in text)
        {
            if (ch == '\n') { KeyEvent(0x0D, false, false); KeyEvent(0x0D, true, false); continue; }
            INPUT i = new INPUT();
            i.type = 1;
            i.u.ki.wScan = ch;
            i.u.ki.dwFlags = 0x0004; // KEYEVENTF_UNICODE
            SendOne(i);
            i.u.ki.dwFlags = 0x0004 | 0x0002;
            SendOne(i);
            Thread.Sleep(8);
        }
    }

    static readonly Dictionary<string, ushort> Vk = new Dictionary<string, ushort>(StringComparer.OrdinalIgnoreCase) {
        {"ctrl", 0x11}, {"control", 0x11}, {"shift", 0x10}, {"alt", 0x12}, {"win", 0x5B},
        {"enter", 0x0D}, {"return", 0x0D}, {"esc", 0x1B}, {"escape", 0x1B}, {"tab", 0x09}, {"space", 0x20},
        {"backspace", 0x08}, {"delete", 0x2E}, {"del", 0x2E}, {"insert", 0x2D}, {"home", 0x24}, {"end", 0x23},
        {"pageup", 0x21}, {"pagedown", 0x22}, {"left", 0x25}, {"up", 0x26}, {"right", 0x27}, {"down", 0x28},
        {"f1", 0x70}, {"f2", 0x71}, {"f3", 0x72}, {"f4", 0x73}, {"f5", 0x74}, {"f6", 0x75}, {"f10", 0x79}, {"f11", 0x7A}, {"f12", 0x7B},
        {",", 0xBC}, {".", 0xBE}, {"-", 0xBD}, {"=", 0xBB}, {"/", 0xBF}
    };
    static readonly HashSet<ushort> Extended = new HashSet<ushort> { 0x2E, 0x2D, 0x24, 0x23, 0x21, 0x22, 0x25, 0x26, 0x27, 0x28, 0x5B };

    static object KeyCombo(string combo, int repeat)
    {
        List<ushort> keys = new List<ushort>();
        foreach (string part in combo.Split('+'))
        {
            string k = part.Trim();
            ushort v;
            if (Vk.TryGetValue(k, out v)) keys.Add(v);
            else if (k.Length == 1 && char.IsLetterOrDigit(k[0])) keys.Add((ushort)char.ToUpperInvariant(k[0]));
            else throw new ArgumentException("unknown key " + k);
        }
        for (int n = 0; n < Math.Max(1, repeat); n++)
        {
            foreach (ushort k in keys) { KeyEvent(k, false, Extended.Contains(k)); Thread.Sleep(15); }
            for (int i = keys.Count - 1; i >= 0; i--) { KeyEvent(keys[i], true, Extended.Contains(keys[i])); Thread.Sleep(15); }
            Thread.Sleep(40);
        }
        return true;
    }

    // ------------------------------------------------------------------ screenshots
    static object Shot(Dictionary<string, object> a)
    {
        string path = S(a, "path", null);
        if (path == null) throw new ArgumentException("path needed");
        IntPtr win = ResolveWindow(a, false);
        int x, y, w, h;
        if (win != IntPtr.Zero)
        {
            if (B(a, "foreground", true)) { Foreground(win); Thread.Sleep(150); }
            RECT r = Bounds(win);
            x = r.Left; y = r.Top; w = r.Right - r.Left; h = r.Bottom - r.Top;
        }
        else { x = 0; y = 0; w = GetSystemMetrics(0); h = GetSystemMetrics(1); }
        if (w < 1 || h < 1) throw new InvalidOperationException("empty capture area");
        using (Bitmap bmp = new Bitmap(w, h))
        {
            using (Graphics g = Graphics.FromImage(bmp)) g.CopyFromScreen(x, y, 0, 0, new Size(w, h));
            Directory.CreateDirectory(Path.GetDirectoryName(path));
            bmp.Save(path, ImageFormat.Png);
        }
        return new Dictionary<string, object> { { "path", path }, { "rect", new int[] { x, y, w, h } } };
    }

    // ------------------------------------------------------------------ processes
    static IEnumerable<string> UserEnvNames()
    {
        using (RegistryKey k = Registry.CurrentUser.OpenSubKey("Environment"))
            if (k != null) foreach (string n in k.GetValueNames()) yield return n;
    }

    static string RegEnv(RegistryKey k, string n)
    {
        object v = k.GetValue(n, null, RegistryValueOptions.DoNotExpandEnvironmentNames);
        if (v == null) return null;
        string s = Convert.ToString(v);
        if (k.GetValueKind(n) == RegistryValueKind.ExpandString) s = Environment.ExpandEnvironmentVariables(s);
        return s;
    }

    // Environment like a process Explorer starts now: this agent's environment with the
    // machine and user variables re-read from the registry (variables the user scope
    // had when the agent started but no longer has are dropped).
    static void FreshEnv(System.Collections.Specialized.StringDictionary env)
    {
        foreach (string n in startUserEnv) if (!string.Equals(n, "PATH", StringComparison.OrdinalIgnoreCase)) env.Remove(n);
        string machinePath = null, userPath = null;
        using (RegistryKey m = Registry.LocalMachine.OpenSubKey(@"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"))
            if (m != null)
                foreach (string n in m.GetValueNames())
                {
                    string v = RegEnv(m, n);
                    if (string.Equals(n, "PATH", StringComparison.OrdinalIgnoreCase)) machinePath = v;
                    else if (v != null) env[n] = v;
                }
        using (RegistryKey u = Registry.CurrentUser.OpenSubKey("Environment"))
            if (u != null)
                foreach (string n in u.GetValueNames())
                {
                    string v = RegEnv(u, n);
                    if (string.Equals(n, "PATH", StringComparison.OrdinalIgnoreCase)) userPath = v;
                    else if (v != null) env[n] = v;
                }
        if (machinePath != null) env["PATH"] = machinePath + (string.IsNullOrEmpty(userPath) ? "" : ";" + userPath);
    }

    static ProcessStartInfo Psi(Dictionary<string, object> a)
    {
        ProcessStartInfo psi = new ProcessStartInfo(S(a, "file", ""), S(a, "args", ""));
        psi.UseShellExecute = false;
        psi.WorkingDirectory = S(a, "cwd", Environment.GetEnvironmentVariable("USERPROFILE"));
        if (B(a, "fresh_env", true)) FreshEnv(psi.EnvironmentVariables);
        Dictionary<string, object> env = D(a, "env");
        if (env != null) foreach (KeyValuePair<string, object> kv in env) psi.EnvironmentVariables[kv.Key] = Convert.ToString(kv.Value);
        foreach (string n in SL(a, "unset")) psi.EnvironmentVariables.Remove(n);
        return psi;
    }

    static object Exec(Dictionary<string, object> a)
    {
        ProcessStartInfo psi = Psi(a);
        psi.RedirectStandardOutput = true;
        psi.RedirectStandardError = true;
        psi.RedirectStandardInput = true;
        psi.CreateNoWindow = true;
        psi.StandardOutputEncoding = new UTF8Encoding(false);
        psi.StandardErrorEncoding = new UTF8Encoding(false);
        int timeout = I(a, "timeout_ms", 120000);
        Stopwatch sw = Stopwatch.StartNew();
        Process p = Process.Start(psi);
        StringBuilder outB = new StringBuilder(), errB = new StringBuilder();
        p.OutputDataReceived += delegate (object o, DataReceivedEventArgs e) { if (e.Data != null) lock (outB) outB.Append(e.Data).Append('\n'); };
        p.ErrorDataReceived += delegate (object o, DataReceivedEventArgs e) { if (e.Data != null) lock (errB) errB.Append(e.Data).Append('\n'); };
        p.BeginOutputReadLine();
        p.BeginErrorReadLine();
        string stdin = S(a, "stdin", null);
        try
        {
            if (stdin != null)
            {
                byte[] bytes = new UTF8Encoding(false).GetBytes(stdin);
                p.StandardInput.BaseStream.Write(bytes, 0, bytes.Length);
                p.StandardInput.BaseStream.Flush();
            }
            p.StandardInput.Close();
        }
        catch (IOException) { }
        bool done = p.WaitForExit(timeout);
        if (!done) { try { KillTree(p.Id); } catch { } }
        else p.WaitForExit();
        Dictionary<string, object> r = new Dictionary<string, object>();
        r["pid"] = p.Id;
        r["code"] = done ? (object)p.ExitCode : null;
        r["timed_out"] = !done;
        lock (outB) r["stdout"] = outB.ToString();
        lock (errB) r["stderr"] = errB.ToString();
        r["ms"] = sw.ElapsedMilliseconds;
        return r;
    }

    static object Start(Dictionary<string, object> a)
    {
        ProcessStartInfo psi = Psi(a);
        string log = S(a, "log", null);
        if (log != null)
        {
            // Through cmd so the child's own stdout/stderr land in a file without this
            // agent holding pipes open for its whole life.
            string inner = "\"" + psi.FileName + "\" " + psi.Arguments;
            psi.FileName = Environment.GetEnvironmentVariable("ComSpec") ?? "cmd.exe";
            psi.Arguments = "/d /s /c \"" + inner + " >\"" + log + "\" 2>&1\"";
            psi.CreateNoWindow = true;
        }
        Process p = Process.Start(psi);
        return new Dictionary<string, object> { { "pid", p.Id } };
    }

    static void KillTree(int pid)
    {
        ProcessStartInfo psi = new ProcessStartInfo("taskkill.exe", "/T /F /PID " + pid);
        psi.UseShellExecute = false; psi.CreateNoWindow = true;
        Process.Start(psi).WaitForExit(15000);
    }

    static object Kill(Dictionary<string, object> a)
    {
        int pid = I(a, "pid", 0);
        if (pid <= 0) throw new ArgumentException("pid needed");
        if (B(a, "tree", true)) KillTree(pid);
        else Process.GetProcessById(pid).Kill();
        return true;
    }

    static object Procs(Dictionary<string, object> a)
    {
        string nameRe = S(a, "name_re", null);
        List<Dictionary<string, object>> o = new List<Dictionary<string, object>>();
        using (ManagementObjectSearcher q = new ManagementObjectSearcher(
            "SELECT ProcessId, ParentProcessId, Name, ExecutablePath, CommandLine, CreationDate, SessionId, HandleCount FROM Win32_Process"))
        {
            foreach (ManagementObject m in q.Get())
            {
                string name = Convert.ToString(m["Name"]);
                if (nameRe != null && !Regex.IsMatch(name, nameRe, RegexOptions.IgnoreCase)) continue;
                Dictionary<string, object> i = new Dictionary<string, object>();
                i["pid"] = Convert.ToInt64(m["ProcessId"]);
                i["ppid"] = Convert.ToInt64(m["ParentProcessId"]);
                i["name"] = name;
                i["path"] = Convert.ToString(m["ExecutablePath"]);
                i["cmdline"] = Convert.ToString(m["CommandLine"]);
                i["session"] = Convert.ToInt64(m["SessionId"]);
                i["handles"] = Convert.ToInt64(m["HandleCount"]);
                string cd = Convert.ToString(m["CreationDate"]);
                if (!string.IsNullOrEmpty(cd)) i["started"] = ManagementDateTimeConverter.ToDateTime(cd).ToUniversalTime().ToString("o");
                o.Add(i);
            }
        }
        return o;
    }

    // Text of a file the desktop user can read (null when missing); max_bytes caps it.
    static object FileRead(Dictionary<string, object> a)
    {
        string path = S(a, "path", "");
        if (!File.Exists(path)) return null;
        int max = I(a, "max_bytes", 4 << 20);
        using (FileStream f = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete))
        {
            int n = (int)Math.Min(f.Length, (long)max);
            byte[] b = new byte[n];
            int off = 0;
            while (off < n) { int r = f.Read(b, off, n - off); if (r <= 0) break; off += r; }
            return new UTF8Encoding(false).GetString(b, 0, off);
        }
    }

    // Mandatory integrity level (RID: 4096 low, 8192 medium, 12288 high, 16384 system) and
    // whether the token is elevated, of another process of this session.
    static object Integrity(int pid)
    {
        IntPtr p = OpenProcess(0x1000 /*PROCESS_QUERY_LIMITED_INFORMATION*/, false, (uint)pid);
        if (p == IntPtr.Zero) throw new System.ComponentModel.Win32Exception();
        IntPtr tok = IntPtr.Zero;
        try
        {
            if (!OpenProcessToken(p, 0x0008 /*TOKEN_QUERY*/, out tok)) throw new System.ComponentModel.Win32Exception();
            int len;
            GetTokenInformation(tok, 25 /*TokenIntegrityLevel*/, IntPtr.Zero, 0, out len);
            IntPtr buf = Marshal.AllocHGlobal(Math.Max(len, 64));
            int rid;
            try
            {
                if (!GetTokenInformation(tok, 25, buf, len, out len)) throw new System.ComponentModel.Win32Exception();
                IntPtr sid = Marshal.ReadIntPtr(buf);
                int count = Marshal.ReadByte(GetSidSubAuthorityCount(sid));
                rid = Marshal.ReadInt32(GetSidSubAuthority(sid, (uint)(count - 1)));
            }
            finally { Marshal.FreeHGlobal(buf); }
            int elevated = -1;
            IntPtr eb = Marshal.AllocHGlobal(4);
            try { int r2; if (GetTokenInformation(tok, 20 /*TokenElevation*/, eb, 4, out r2)) elevated = Marshal.ReadInt32(eb); }
            finally { Marshal.FreeHGlobal(eb); }
            string level = rid >= 16384 ? "system" : rid >= 12288 ? "high" : rid >= 8192 ? "medium" : rid >= 4096 ? "low" : "untrusted";
            return new Dictionary<string, object> { { "pid", pid }, { "rid", rid }, { "level", level }, { "elevated", elevated == 1 } };
        }
        finally
        {
            if (tok != IntPtr.Zero) CloseHandle(tok);
            CloseHandle(p);
        }
    }

    static object UserEnv()
    {
        Dictionary<string, object> o = new Dictionary<string, object>();
        using (RegistryKey u = Registry.CurrentUser.OpenSubKey("Environment"))
            if (u != null) foreach (string n in u.GetValueNames()) o[n] = Convert.ToString(u.GetValue(n, null, RegistryValueOptions.DoNotExpandEnvironmentNames));
        return o;
    }

    static object Broadcast()
    {
        IntPtr res;
        SendMessageTimeout(new IntPtr(0xffff), 0x001A /*WM_SETTINGCHANGE*/, IntPtr.Zero, "Environment", 0x0002, 5000, out res);
        return true;
    }

    // ------------------------------------------------------------------ CDP (desktop app renderer)
    static object CdpList(int port)
    {
        using (WebClient wc = new WebClient())
        {
            wc.Encoding = Encoding.UTF8;
            return Json.DeserializeObject(wc.DownloadString("http://127.0.0.1:" + port + "/json/list"));
        }
    }

    static string CdpTarget(int port, string prefix)
    {
        object[] ts = CdpList(port) as object[];
        if (ts != null)
            foreach (object o in ts)
            {
                Dictionary<string, object> t = o as Dictionary<string, object>;
                if (t == null) continue;
                string url = S(t, "url", "");
                if (S(t, "type", "") == "page" && url.StartsWith(prefix) && url.IndexOf("avatar-overlay") < 0
                    && url.IndexOf("quick-chat") < 0 && url.IndexOf("prewarm") < 0)
                    return S(t, "webSocketDebuggerUrl", null);
            }
        throw new InvalidOperationException("main renderer target not found");
    }

    // One CDP command on the main renderer: {port, method, params, timeout_ms, url_prefix}.
    static object CdpCall(Dictionary<string, object> a)
    {
        int port = I(a, "port", 0);
        string ws = CdpTarget(port, S(a, "url_prefix", "app://-/index.html"));
        int timeout = I(a, "timeout_ms", 30000);
        using (ClientWebSocket c = new ClientWebSocket())
        using (CancellationTokenSource cts = new CancellationTokenSource(timeout))
        {
            c.Options.KeepAliveInterval = TimeSpan.Zero;
            c.ConnectAsync(new Uri(ws), cts.Token).Wait();
            Dictionary<string, object> msg = new Dictionary<string, object>();
            msg["id"] = 1;
            msg["method"] = S(a, "method", "");
            Dictionary<string, object> pr = D(a, "params");
            msg["params"] = pr ?? new Dictionary<string, object>();
            byte[] b = Encoding.UTF8.GetBytes(Json.Serialize(msg));
            c.SendAsync(new ArraySegment<byte>(b), WebSocketMessageType.Text, true, cts.Token).Wait();
            byte[] buf = new byte[1 << 16];
            while (true)
            {
                MemoryStream ms = new MemoryStream();
                WebSocketReceiveResult rr;
                do
                {
                    rr = c.ReceiveAsync(new ArraySegment<byte>(buf), cts.Token).Result;
                    if (rr.MessageType == WebSocketMessageType.Close) throw new InvalidOperationException("CDP socket closed");
                    ms.Write(buf, 0, rr.Count);
                } while (!rr.EndOfMessage);
                Dictionary<string, object> m = Json.DeserializeObject(Encoding.UTF8.GetString(ms.ToArray())) as Dictionary<string, object>;
                if (m == null || !m.ContainsKey("id") || Convert.ToInt32(m["id"]) != 1) continue;
                try { c.CloseAsync(WebSocketCloseStatus.NormalClosure, "", CancellationToken.None).Wait(2000); } catch { }
                if (m.ContainsKey("error")) throw new InvalidOperationException("CDP " + msg["method"] + ": " + Json.Serialize(m["error"]));
                return m.ContainsKey("result") ? m["result"] : null;
            }
        }
    }
}
