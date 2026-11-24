/* aetherrv-panel-wpe: the panel's kiosk launcher for WPE WebKit (WPE Platform API), in place of MiniBrowser.
 *
 * One full-screen web view on the compositor's display (sway, packaging/panel/sway.conf) and nothing else: no
 * browser chrome, no context menu, no new windows, no navigation away from the UI's own origin. The page colour is
 * the view's background, so a slow first paint can't flash white. The UI keeps per-screen preferences in
 * localStorage, so the profile is persistent (AETHERRV_PANEL_PROFILE, default $XDG_DATA_HOME/aetherrv-panel-wpe).
 *
 *   aetherrv-panel-wpe [URL]       URL: argv[1], else $AETHERRV_PANEL_URL, else http://127.0.0.1:8080/
 *
 * Exit status: 0 when the window is closed or on SIGTERM/SIGINT; 2 if the web process crashes or is killed for its
 * memory, 3 if it stops responding for AETHERRV_PANEL_HANG_S seconds (default 30; 0 = never; WebKit notices a hang
 * only when it has something to deliver, e.g. a touch: a frozen page is restarted after the next tap), 1 on a
 * start-up error, 4 when the compositor goes away (WPEDisplay "disconnected": the Wayland connection was lost, e.g.
 * sway stopped by a restart of the panel unit; without this the launcher would outlive its session). launch.sh then
 * ends the sway session and the panel unit (Restart=always) starts everything again.
 * Build: cc -O2 -o aetherrv-panel-wpe aetherrv-panel-wpe.c $(pkg-config --cflags --libs wpe-webkit-2.0)
 */
#include <glib-unix.h>
#include <stdio.h>
#include <string.h>
#include <wpe/webkit.h>

#define PAGE_COLOUR "#0b0c0f" /* ui/src/styles: --page */

static GMainLoop *loop;
static int status;
static char *origin;     /* scheme://host:port of the start URL: the only place the view may navigate to */
static guint hang_timer;
static guint hang_s = 30;

static void quit(int code)
{
    if (!status)
        status = code;
    g_main_loop_quit(loop);
}

static char *origin_of(const char *uri)
{
    g_autoptr(GUri) u = g_uri_parse(uri, G_URI_FLAGS_NONE, NULL);
    if (!u || !g_uri_get_host(u))
        return NULL;
    int port = g_uri_get_port(u);
    if (port < 0)
        port = !g_strcmp0(g_uri_get_scheme(u), "https") ? 443 : 80;
    return g_strdup_printf("%s://%s:%d", g_uri_get_scheme(u), g_uri_get_host(u), port);
}

static gboolean on_decide_policy(WebKitWebView *view, WebKitPolicyDecision *decision, WebKitPolicyDecisionType type, gpointer data)
{
    (void)view; (void)data;
    if (type == WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION) {           /* window.open, target=_blank */
        webkit_policy_decision_ignore(decision);
        return TRUE;
    }
    if (type != WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION)
        return FALSE;
    WebKitNavigationAction *a = webkit_navigation_policy_decision_get_navigation_action(WEBKIT_NAVIGATION_POLICY_DECISION(decision));
    const char *uri = webkit_uri_request_get_uri(webkit_navigation_action_get_request(a));
    g_autofree char *o = uri ? origin_of(uri) : NULL;
    if (uri && (g_str_has_prefix(uri, "about:") || (o && !strcmp(o, origin))))
        return FALSE;                                                       /* the UI itself, or a reload */
    g_printerr("aetherrv-panel-wpe: navigation to %s refused (only %s)\n", uri ? uri : "?", origin);
    webkit_policy_decision_ignore(decision);
    return TRUE;
}

static void on_web_process_terminated(WebKitWebView *view, WebKitWebProcessTerminationReason reason, gpointer data)
{
    (void)view; (void)data;
    g_printerr("aetherrv-panel-wpe: web process %s: exiting so that the panel restarts\n",
        reason == WEBKIT_WEB_PROCESS_EXCEEDED_MEMORY_LIMIT ? "exceeded its memory limit" : reason == WEBKIT_WEB_PROCESS_CRASHED ? "crashed" : "terminated");
    quit(2);
}

static gboolean on_hang(gpointer data)
{
    (void)data;
    hang_timer = 0;
    g_printerr("aetherrv-panel-wpe: web process unresponsive for %u s: exiting so that the panel restarts\n", hang_s);
    quit(3);
    return G_SOURCE_REMOVE;
}

static void on_responsive(WebKitWebView *view, GParamSpec *p, gpointer data)
{
    (void)p; (void)data;
    if (webkit_web_view_get_is_web_process_responsive(view)) {
        if (hang_timer)
            g_source_remove(hang_timer);
        hang_timer = 0;
    } else if (!hang_timer && hang_s)
        hang_timer = g_timeout_add_seconds(hang_s, on_hang, NULL);
}

static gboolean on_context_menu(WebKitWebView *v, WebKitContextMenu *m, WebKitHitTestResult *h, gpointer d)
{
    (void)v; (void)m; (void)h; (void)d;
    return TRUE;                                                            /* none: a long press is not a menu */
}

static void on_close(WebKitWebView *view, gpointer data)
{
    (void)view; (void)data;
    quit(0);
}

static void on_display_disconnected(WPEDisplay *display, GError *error, gpointer data)
{
    (void)display; (void)data;
    g_printerr("aetherrv-panel-wpe: the compositor went away (%s): exiting\n", error ? error->message : "disconnected");
    quit(4);
}

static gboolean on_signal(gpointer data)
{
    (void)data;
    quit(0);
    return G_SOURCE_REMOVE;
}

int main(int argc, char **argv)
{
    const char *url = argc > 1 ? argv[1] : g_getenv("AETHERRV_PANEL_URL");
    if (!url || !*url)
        url = "http://127.0.0.1:8080/";
    origin = origin_of(url);
    if (!origin) {
        g_printerr("aetherrv-panel-wpe: not a URL: %s\n", url);
        return 1;
    }
    const char *h = g_getenv("AETHERRV_PANEL_HANG_S");
    if (h && *h)
        hang_s = (guint)g_ascii_strtoull(h, NULL, 10);
    const char *prof = g_getenv("AETHERRV_PANEL_PROFILE");
    g_autofree char *dir = prof && *prof ? g_strdup(prof) : g_build_filename(g_get_user_data_dir(), "aetherrv-panel-wpe", NULL);
    g_autofree char *data_dir = g_build_filename(dir, "data", NULL);
    g_autofree char *cache_dir = g_build_filename(dir, "cache", NULL);

    loop = g_main_loop_new(NULL, FALSE);
    g_unix_signal_add(SIGTERM, on_signal, NULL);
    g_unix_signal_add(SIGINT, on_signal, NULL);

    WPEDisplay *display = wpe_display_get_default();                       /* sway's, through WAYLAND_DISPLAY */
    if (!display) {
        g_printerr("aetherrv-panel-wpe: no display\n");
        return 1;
    }
    g_signal_connect(display, "disconnected", G_CALLBACK(on_display_disconnected), NULL);
    g_autoptr(WebKitNetworkSession) session = webkit_network_session_new(data_dir, cache_dir);
    g_autoptr(WebKitWebsitePolicies) policies = webkit_website_policies_new_with_policies("autoplay", WEBKIT_AUTOPLAY_ALLOW, NULL);
    g_autoptr(WebKitSettings) settings = webkit_settings_new();
    webkit_settings_set_media_playback_requires_user_gesture(settings, FALSE);  /* alert tones before the first touch */
    WebKitWebView *view = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", display, "network-session", session,
        "website-policies", policies, "settings", settings, NULL));

    WebKitColor bg;
    webkit_color_parse(&bg, PAGE_COLOUR);
    webkit_web_view_set_background_color(view, &bg);
    WPEView *wv = webkit_web_view_get_wpe_view(view);
    WPEToplevel *top = wv ? wpe_view_get_toplevel(wv) : NULL;
    if (top)
        wpe_toplevel_fullscreen(top);
    g_signal_connect(view, "decide-policy", G_CALLBACK(on_decide_policy), NULL);
    g_signal_connect(view, "web-process-terminated", G_CALLBACK(on_web_process_terminated), NULL);
    g_signal_connect(view, "notify::is-web-process-responsive", G_CALLBACK(on_responsive), NULL);
    g_signal_connect(view, "context-menu", G_CALLBACK(on_context_menu), NULL);
    g_signal_connect(view, "close", G_CALLBACK(on_close), NULL);
    if (wv)
        g_signal_connect_swapped(wv, "closed", G_CALLBACK(on_close), view);   /* the compositor closed the window */
    webkit_web_view_load_uri(view, url);

    g_main_loop_run(loop);
    g_object_unref(view);
    g_main_loop_unref(loop);
    g_free(origin);
    return status;
}
