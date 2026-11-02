// File: ballista/sdks/java/src/test/java/ballista/client/BundleRulesTest.java
package ballista.client;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ballista.client.exceptions.ConfigurationException;
import ballista.client.internal.Fields;
import ballista.client.internal.Rules;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class BundleRulesTest {
    @Test
    void pathRules() {
        for (String p : List.of("a", "scenarios/s.rhai", "a/b/c/d/e/f/g/h", "_x/-y/z.v1.csv")) {
            assertTrue(Rules.bundlePath(p), p);
        }
        for (String p : List.of("", "/abs", "a/", "./a", "../a", "a/../b", ".env", "a/.git/config", "a\\b", "a b", "a/b/c/d/e/f/g/h/i",
                "inline/x", "plan.toml", "bundle.json", "a".repeat(201), "é.txt", "a\u0000b")) {
            assertFalse(Rules.bundlePath(p), p);
        }
    }

    @Test
    void sizeLimits() {
        Bundle b = new Bundle("name = \"x\"");
        assertThrows(ConfigurationException.class, () -> b.addBytes("big", new byte[Bundle.MAX_FILE_BYTES + 1]));
        byte[] chunk = new byte[Bundle.MAX_FILE_BYTES];
        for (int i = 0; i < 4; i++) {
            b.addBytes("part" + i, chunk);
        }
        assertThrows(ConfigurationException.class, () -> b.addBytes("part4", new byte[1]));
        assertDoesNotThrow(() -> b.addBytes("part0", chunk), "replacing a file must not count it twice");

        Bundle many = new Bundle("name = \"x\"");
        for (int i = 0; i < Bundle.MAX_FILES; i++) {
            many.addText("f" + i, "x");
        }
        assertThrows(ConfigurationException.class, () -> many.addText("one-more", "x"));
        assertDoesNotThrow(() -> many.addText("f0", "replace"));
    }

    @Test
    void copiesInput() {
        Bundle b = new Bundle("name = \"x\"");
        byte[] data = "abc".getBytes();
        b.addBytes("f", data);
        data[0] = 'z';
        Fields files = Fields.of(b.toPayload(), "payload").obj("files");
        assertEquals("YWJj", files.str("f"));
        Map<String, String> labels = new HashMap<>(Map.of("k", "v"));
        Bundle b2 = new Bundle("name = \"x\"", labels);
        labels.put("k", "changed");
        assertEquals("v", Fields.of(b2.toPayload(), "payload").obj("labels").str("k"));
        Map<String, Object> doc = new HashMap<>(Map.of("name", "doc"));
        Bundle b3 = new Bundle(doc);
        doc.put("name", "changed");
        assertEquals("doc", Fields.of(b3.toPayload(), "payload").obj("plan").str("name"));
    }

    @Test
    void localFiles(@TempDir Path dir) throws Exception {
        Path script = dir.resolve("s.rhai");
        Files.writeString(script, "fn run(s, r) {}");
        Bundle b = new Bundle(PlanTest.shopPlan("http://127.0.0.1:8080/"));
        b.addFile("scenarios/s.rhai", script);
        assertThrows(ConfigurationException.class, () -> b.addFile("x", dir.resolve("missing")));
        assertThrows(ConfigurationException.class, () -> b.addFile("x", dir));
        Path link = dir.resolve("link");
        Files.createSymbolicLink(link, script);
        b.addFile("scenarios/link.rhai", link);
        assertEquals(Map.of("scenarios/link.rhai", 15, "scenarios/s.rhai", 15), b.files());
        assertEquals("Bundle(plan=document, files=2)", b.toString());
        assertEquals("Bundle(plan=text, files=0)", new Bundle("name = \"x\"").toString());
    }

    @Test
    void planForms() {
        for (Object bad : new Object[] {null, Map.of(), "", "  \n", "#".repeat(Bundle.MAX_PLAN_BYTES + 1), 1, new byte[] {1}}) {
            assertThrows(ConfigurationException.class, () -> new Bundle(bad));
        }
        assertThrows(ConfigurationException.class, () -> new Bundle("name = \"x\"", Map.of("bad key", "v")));
    }
}
