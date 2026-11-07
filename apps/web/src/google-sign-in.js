// The native Google sign-in of the Android app (#670).
//
// Google refuses its consent screen inside a WebView (`403
// disallowed_useragent`), so the app's /login and /register show a button
// instead of the link to `/auth/google/start` (`routes::auth::
// google_actions`), and this script, loaded right after it on the app's
// pages only. It reaches the Social Login plugin (Credential Manager)
// through the global the Capacitor bridge puts in the page
// (`window.Capacitor.Plugins`), since the site is not bundled into the app
// (apps/mobile/README.md): on Android the plugin is native, and no script
// of Google's comes into the page.
//
// On a touch: a nonce from apps/api (`POST /auth/google/nonce`, #669,
// which also sets its flow cookie and names the web client the token must
// be issued for), the system's account picker with that nonce, the plugin
// told to forget the token it keeps on the phone, the token posted to
// apps/api (`POST /auth/google/id-token`), which opens the session in this
// WebView, then the page apps/api names: the app, or the login page with
// the reason of a refusal (`?google=`, the same copy as on the web). The
// picker closed by the member puts the page back as it was, silently.
//
// The pure part (`redirectTarget`, `isCancelled`, `message`) and `signIn`
// are tested by apps/mobile/scripts/google-sign-in.test.mjs, which runs
// this file in a context with no `window`: only `start` touches the page.
var MHomeGoogleSignIn = (function () {
  "use strict";

  // The plugin's code for a picker the member closed.
  var CANCELLED = "USER_CANCELLED";

  // The path apps/api sends the page to, when it is one of this site's:
  // `/…`, but neither `//host` nor `/\host`, which browsers read as
  // another site. `null` for anything else.
  function redirectTarget(body) {
    var to = body && typeof body === "object" ? body.redirect : null;
    if (typeof to !== "string" || to.charAt(0) !== "/") return null;
    var second = to.charAt(1);
    return second === "/" || second === "\\" ? null : to;
  }

  function isCancelled(error) {
    return !!error && error.code === CANCELLED;
  }

  function post(env, path, body) {
    var init = {
      method: "POST",
      credentials: "include",
      headers: { Accept: "application/json" }
    };
    if (body !== undefined) {
      init.headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    return env.fetch(env.api + path, init);
  }

  // The JSON of an answer, `null` when it has none.
  function json(res) {
    return Promise.resolve()
      .then(function () { return res.json(); })
      .then(null, function () { return null; });
  }

  function nonEmpty(value) {
    return typeof value === "string" && value !== "";
  }

  // A failure that says what the page shows.
  function stop(state) {
    var error = new Error(state);
    error.outcome = { state: state };
    return error;
  }

  // One sign-in. `env`: `plugin` (Social Login), `fetch`, `api` (the
  // browser's base of apps/api, `/api`). Resolves to what happened, never
  // rejects:
  // - `redirect`: `to`, the page to go to;
  // - `cancelled`: the member closed the picker;
  // - `throttled`: apps/api's limit for this address is spent;
  // - `failed`: anything else.
  function signIn(env) {
    var plugin = env.plugin;
    return Promise.resolve()
      .then(function () { return post(env, "/auth/google/nonce"); })
      .then(function (res) {
        if (res.status === 429) throw stop("throttled");
        if (!res.ok) throw stop("failed");
        return json(res);
      })
      .then(function (body) {
        if (!body || !nonEmpty(body.nonce) || !nonEmpty(body.client_id)) throw stop("failed");
        // Online: the ID token only. No scope beyond the sign-in's, so
        // nothing to authorise after the picker either.
        return Promise.resolve(plugin.initialize({
          google: {
            webClientId: body.client_id,
            mode: "online",
            skipAuthOnlyAuthorizationAfterSignIn: true
          }
        })).then(function () {
          return plugin.login({ provider: "google", options: { nonce: body.nonce } });
        });
      })
      .then(function (answer) {
        var token = answer && answer.result && answer.result.idToken;
        if (!nonEmpty(token)) throw stop("failed");
        // The plugin keeps the token on the phone; the session is what
        // the app needs, the token goes to apps/api only.
        return Promise.resolve()
          .then(function () { return plugin.logout({ provider: "google" }); })
          .then(null, function () { return undefined; })
          .then(function () { return post(env, "/auth/google/id-token", { id_token: token }); });
      })
      .then(function (res) {
        if (res.status === 429) throw stop("throttled");
        if (!res.ok) throw stop("failed");
        return json(res);
      })
      .then(function (body) {
        var to = redirectTarget(body);
        if (!to) throw stop("failed");
        return { state: "redirect", to: to };
      })
      .then(null, function (error) {
        if (error && error.outcome) return error.outcome;
        if (isCancelled(error)) return { state: "cancelled" };
        return { state: "failed" };
      });
  }

  // What the page says of an outcome, `null` for nothing.
  function message(outcome) {
    switch (outcome && outcome.state) {
      case "throttled":
        return "Trop de tentatives de connexion. Merci de réessayer dans quelques minutes.";
      case "failed":
        return "La connexion avec Google n'a pas abouti. Merci de réessayer.";
      default:
        return null;
    }
  }

  // In the app's page: show the button when the plugin is there, and sign
  // in on its touch. Without the plugin (an APK built before #670), a
  // notice says to update; the button stays hidden.
  function start(win) {
    var doc = win.document;
    var button = doc.getElementById("google-native");
    if (!button) return;
    var cap = win.Capacitor;
    var plugin = cap && cap.Plugins && cap.Plugins.SocialLogin;
    if (!plugin) {
      var unsupported = doc.getElementById("google-native-unsupported");
      if (unsupported) unsupported.hidden = false;
      return;
    }
    var error = doc.getElementById("google-native-error");
    button.hidden = false;
    button.addEventListener("click", function () {
      button.disabled = true;
      if (error) error.hidden = true;
      signIn({
        plugin: plugin,
        fetch: function (url, init) { return win.fetch(url, init); },
        api: button.getAttribute("data-api") || ""
      }).then(function (outcome) {
        if (outcome.state === "redirect") {
          win.location.assign(outcome.to);
          return;
        }
        button.disabled = false;
        var text = message(outcome);
        if (error && text) {
          error.textContent = text;
          error.hidden = false;
        }
      });
    });
  }

  return {
    redirectTarget: redirectTarget,
    isCancelled: isCancelled,
    signIn: signIn,
    message: message,
    start: start
  };
})();

if (typeof window !== "undefined" && window.document) MHomeGoogleSignIn.start(window);
