package fr.watallo.mhome;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/** The pure decisions of the cooking timers (#1017), off the device. */
public class CookTimerPlanTest {

    private static final long NOW = 1_800_000_000_000L;
    private static final String SITE = "https://staging.watallo.fr";

    // -- when it rings ------------------------------------------------------

    @Test
    public void aTimerToComeRingsAtItsEnd() {
        assertEquals(NOW + 300_000, CookTimerPlan.triggerAt(NOW + 300_000, NOW));
        assertEquals(NOW + 1, CookTimerPlan.triggerAt(NOW + 1, NOW));
    }

    @Test
    public void aTimerJustOverRingsAtOnce() {
        assertEquals(NOW, CookTimerPlan.triggerAt(NOW, NOW));
        assertEquals(NOW, CookTimerPlan.triggerAt(NOW - CookTimerPlan.LATE_MS, NOW));
    }

    @Test
    public void aTimerLongOverDoesNotRing() {
        assertEquals(CookTimerPlan.NEVER, CookTimerPlan.triggerAt(NOW - CookTimerPlan.LATE_MS - 1, NOW));
        assertEquals(CookTimerPlan.NEVER, CookTimerPlan.triggerAt(0, NOW));
        assertEquals(CookTimerPlan.NEVER, CookTimerPlan.triggerAt(-5, NOW));
    }

    @Test
    public void lateIsAMinute() {
        assertEquals(60_000L, CookTimerPlan.LATE_MS);
    }

    // -- exact or not -------------------------------------------------------

    @Test
    public void beforeAndroid12AlarmsAreExactWithoutAsking() {
        assertTrue(CookTimerPlan.exact(24, false));
        assertTrue(CookTimerPlan.exact(30, false));
    }

    @Test
    public void fromAndroid12AnAlarmIsExactOnlyWhenGranted() {
        assertTrue(CookTimerPlan.exact(31, true));
        assertFalse(CookTimerPlan.exact(31, false));
        assertTrue(CookTimerPlan.exact(36, true));
        assertFalse(CookTimerPlan.exact(36, false));
    }

    // -- full screen or a notification to touch -----------------------------

    @Test
    public void beforeAndroid14TheFullScreenIntentIsDeclaredAndGranted() {
        assertTrue(CookTimerPlan.fullScreen(24, false));
        assertTrue(CookTimerPlan.fullScreen(33, false));
    }

    @Test
    public void fromAndroid14TheFullScreenIntentNeedsTheGrant() {
        assertTrue(CookTimerPlan.fullScreen(34, true));
        assertFalse(CookTimerPlan.fullScreen(34, false));
        assertFalse(CookTimerPlan.fullScreen(36, false));
    }

    // -- the words ----------------------------------------------------------

    @Test
    public void theTitleIsTheRecipesName() {
        assertEquals("Pâtes à la carbonara", CookTimerPlan.title("Pâtes à la carbonara"));
        assertEquals("Pâtes à la carbonara", CookTimerPlan.title("  Pâtes \n à   la\tcarbonara "));
    }

    @Test
    public void withoutANameTheTitleIsGeneric() {
        assertEquals("Minuteur de cuisine", CookTimerPlan.title(null));
        assertEquals("Minuteur de cuisine", CookTimerPlan.title(""));
        assertEquals("Minuteur de cuisine", CookTimerPlan.title(" \n\t "));
    }

    @Test
    public void aLongNameIsCut() {
        String name = "a".repeat(200);
        String title = CookTimerPlan.title(name);
        assertEquals(CookTimerPlan.MAX_LENGTH, title.length());
        assertTrue(title.endsWith("…"));
        assertEquals("a".repeat(CookTimerPlan.MAX_LENGTH), CookTimerPlan.title("a".repeat(CookTimerPlan.MAX_LENGTH)));
    }

    @Test
    public void aCutNeverSplitsACharacter() {
        // An emoji is two chars: cut between them, it would show a broken one.
        String name = "a".repeat(CookTimerPlan.MAX_LENGTH - 2) + "🍝" + "b".repeat(20);
        String title = CookTimerPlan.title(name);
        assertEquals("a".repeat(CookTimerPlan.MAX_LENGTH - 2) + "…", title);
        String fits = "a".repeat(CookTimerPlan.MAX_LENGTH - 3) + "🍝" + "b".repeat(20);
        assertEquals("a".repeat(CookTimerPlan.MAX_LENGTH - 3) + "🍝…", CookTimerPlan.title(fits));
    }

    @Test
    public void noBreakSpacesAreSpacesToo() {
        assertEquals("Minuteur de cuisine", CookTimerPlan.title(" "));
        assertEquals("Minuteur de cuisine", CookTimerPlan.title("     "));
        assertEquals("Pâtes à la carbonara", CookTimerPlan.title(" Pâtes  à la carbonara "));
        assertEquals("Minuteur terminé", CookTimerPlan.text(" "));
    }

    @Test
    public void theTextAlwaysSaysTheTimerIsOver() {
        assertEquals("Minuteur terminé", CookTimerPlan.text(null));
        assertEquals("Minuteur terminé", CookTimerPlan.text("  "));
        assertEquals("Minuteur terminé : cuisson des pâtes", CookTimerPlan.text(" cuisson  des pâtes "));
    }

    @Test
    public void aLongTextIsCut() {
        String text = CookTimerPlan.text("b".repeat(200));
        assertTrue(text.startsWith("Minuteur terminé : b"));
        assertEquals("Minuteur terminé : ".length() + CookTimerPlan.MAX_LENGTH, text.length());
        assertTrue(text.endsWith("…"));
    }

    // -- what a touch opens -------------------------------------------------

    @Test
    public void aPathOfTheSiteIsKept() {
        assertEquals("/recettes/12/cuisine", CookTimerPlan.sitePath("/recettes/12/cuisine", SITE));
        assertEquals("/recettes/12?mode=cuisine#etape-3", CookTimerPlan.sitePath("/recettes/12?mode=cuisine#etape-3", SITE));
    }

    @Test
    public void anAddressOfTheSiteGivesItsPath() {
        assertEquals("/recettes/12/cuisine", CookTimerPlan.sitePath(SITE + "/recettes/12/cuisine", SITE));
        assertEquals("/recettes/12?mode=cuisine", CookTimerPlan.sitePath(SITE + "/recettes/12?mode=cuisine", SITE));
        assertEquals("/", CookTimerPlan.sitePath(SITE, SITE));
        assertEquals("/recettes/12#etape-3", CookTimerPlan.sitePath(SITE + "/recettes/12#etape-3", SITE));
        assertEquals("/recettes/12?mode=cuisine#etape-3", CookTimerPlan.sitePath(SITE + "/recettes/12?mode=cuisine#etape-3", SITE));
    }

    @Test
    public void anythingElseIsRefused() {
        assertNull(CookTimerPlan.sitePath("https://ailleurs.example/recettes/12", SITE));
        assertNull(CookTimerPlan.sitePath("http://staging.watallo.fr/recettes/12", SITE));
        assertNull(CookTimerPlan.sitePath("https://staging.watallo.fr:8443/recettes", SITE));
        assertNull(CookTimerPlan.sitePath("https://staging.watallo.fr.ailleurs.example/", SITE));
        assertNull(CookTimerPlan.sitePath("https://user@staging.watallo.fr/", SITE));
        assertNull(CookTimerPlan.sitePath("//ailleurs.example/recettes", SITE));
        assertNull(CookTimerPlan.sitePath("/\\ailleurs.example", SITE));
        assertNull(CookTimerPlan.sitePath("javascript:alert(1)", SITE));
        assertNull(CookTimerPlan.sitePath("recettes/12", SITE));
        assertNull(CookTimerPlan.sitePath("/recettes/12\n", SITE));
        assertNull(CookTimerPlan.sitePath("/recettes/12", null));
        assertNull(CookTimerPlan.sitePath(null, SITE));
        assertNull(CookTimerPlan.sitePath("", SITE));
    }

    // -- the id -------------------------------------------------------------

    @Test
    public void anIdIsShortAndPlain() {
        assertTrue(CookTimerPlan.validId("t1"));
        assertTrue(CookTimerPlan.validId("recette-12_etape-3"));
        assertTrue(CookTimerPlan.validId("a".repeat(64)));
        assertFalse(CookTimerPlan.validId(null));
        assertFalse(CookTimerPlan.validId(""));
        assertFalse(CookTimerPlan.validId("a".repeat(65)));
        assertFalse(CookTimerPlan.validId("a/b"));
        assertFalse(CookTimerPlan.validId("a b"));
        assertFalse(CookTimerPlan.validId("é"));
    }
}
