package fr.watallo.mhome;

import java.net.URI;
import java.net.URISyntaxException;

/**
 * The decisions of the cooking timers (#1017), apart from Android so that
 * they run as plain JUnit tests (src/test, CookTimerPlanTest): when a timer
 * rings, whether its alarm is exact, whether it may open full screen, its
 * words, and what a touch opens.
 */
final class CookTimerPlan {

    /** A timer over by this much or less when scheduled still rings, at once. */
    static final long LATE_MS = 60_000;
    /** {@link #triggerAt}'s answer for a timer long over: it does not ring. */
    static final long NEVER = -1;
    /** The longest name and label shown, the ellipsis included. */
    static final int MAX_LENGTH = 80;

    /** The title when the page gives no recipe name. */
    static final String GENERIC_TITLE = "Minuteur de cuisine";
    /** The text of every timer that rings. */
    static final String OVER = "Minuteur terminé";

    /** The longest id: it goes into an intent's data and a notification's tag. */
    static final int MAX_ID_LENGTH = 64;

    private CookTimerPlan() {}

    /**
     * When the timer ending at {@code endAt} rings, both in ms since the
     * epoch: at its end if it is to come; now if it ended up to
     * {@link #LATE_MS} ago (the page scheduled it late); {@link #NEVER} if
     * it ended before that, or {@code endAt} is no time at all.
     */
    static long triggerAt(long endAt, long now) {
        if (endAt <= 0) {
            return NEVER;
        }
        if (endAt > now) {
            return endAt;
        }
        return now - endAt <= LATE_MS ? now : NEVER;
    }

    /**
     * Whether the alarm is exact: always below Android 12 (API 31); from
     * it on, only when the member grants « Alarmes et rappels »
     * ({@code AlarmManager.canScheduleExactAlarms()}). Otherwise inexact,
     * as the reminders (apps/web/src/local-reminders.js).
     */
    static boolean exact(int sdk, boolean canScheduleExactAlarms) {
        return sdk < 31 || canScheduleExactAlarms;
    }

    /**
     * Whether the notification opens full screen: the manifest's
     * USE_FULL_SCREEN_INTENT is granted below Android 14 (API 34); from it
     * on, only if {@code NotificationManager.canUseFullScreenIntent()}.
     * Otherwise a high-priority notification to touch.
     */
    static boolean fullScreen(int sdk, boolean canUseFullScreenIntent) {
        return sdk < 34 || canUseFullScreenIntent;
    }

    /** The notification's title: the recipe's name, or {@link #GENERIC_TITLE}. */
    static String title(String recipe) {
        String name = clean(recipe);
        return name.isEmpty() ? GENERIC_TITLE : name;
    }

    /** The notification's text: {@link #OVER}, then the page's label if any. */
    static String text(String label) {
        String words = clean(label);
        return words.isEmpty() ? OVER : OVER + " : " + words;
    }

    /**
     * {@code url} as a path of the site at {@code origin}
     * ({@code https://host[:port]}), with its query and fragment: a path
     * ({@code /…}, not {@code //…}) as it is, or an address of that very
     * origin. Null for anything else (another site, another scheme, a
     * relative path, a backslash or a control character), and for none.
     */
    static String sitePath(String url, String origin) {
        if (url == null || url.isEmpty() || origin == null) {
            return null;
        }
        for (int i = 0; i < url.length(); i++) {
            char c = url.charAt(i);
            if (c < 0x20 || c == 0x7f || c == '\\') {
                return null;
            }
        }
        if (url.startsWith("/")) {
            return url.startsWith("//") ? null : url;
        }
        URI uri;
        try {
            uri = new URI(url);
        } catch (URISyntaxException e) {
            return null;
        }
        if (!"https".equals(uri.getScheme()) || uri.getRawUserInfo() != null || !origin.equals("https://" + uri.getRawAuthority())) {
            return null;
        }
        String path = uri.getRawPath() == null || uri.getRawPath().isEmpty() ? "/" : uri.getRawPath();
        if (uri.getRawQuery() != null) {
            path += "?" + uri.getRawQuery();
        }
        if (uri.getRawFragment() != null) {
            path += "#" + uri.getRawFragment();
        }
        return path;
    }

    /** Whether {@code id} names a timer: 1 to 64 of {@code A-Z a-z 0-9 _ -}. */
    static boolean validId(String id) {
        if (id == null || id.isEmpty() || id.length() > MAX_ID_LENGTH) {
            return false;
        }
        for (int i = 0; i < id.length(); i++) {
            char c = id.charAt(i);
            boolean plain = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c == '-';
            if (!plain) {
                return false;
            }
        }
        return true;
    }

    /** {@code words} on one line, single-spaced, cut to {@link #MAX_LENGTH}; "" for none. */
    private static String clean(String words) {
        if (words == null) {
            return "";
        }
        String line = words.trim().replaceAll("\\s+", " ");
        return line.length() <= MAX_LENGTH ? line : line.substring(0, MAX_LENGTH - 1) + "…";
    }
}
