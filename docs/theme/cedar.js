/* Cedar syntax highlighting for the documentation site, by Cedar's own
   grammar (cedar-policy/highlightjs-cedar, vendored beside this file).
   The grammar is written for highlight.js 11 and mdBook bundles 10.1.1,
   which cannot load it, so a private highlight.js 11 core highlights the
   ```cedar blocks and nothing else: mdBook's highlighter and every other
   block stay as they are. book.js has already run and left the Cedar
   blocks as plain text, their language being unknown to it. Loaded through
   book.toml's additional-js; the vendored modules are the copies mdBook
   makes of everything under docs/, found from mdBook's path_to_root. */

(function () {
    "use strict";

    const blocks = document.querySelectorAll("code.language-cedar");
    if (blocks.length === 0) {
        return;
    }

    const theme = new URL(path_to_root + "theme/", document.baseURI);
    Promise.all([
        import(new URL("highlightjs/core.min.js", theme)),
        import(new URL("highlightjs-cedar/hljs-cedar.esm.js", theme)),
    ]).then(function ([core, cedar]) {
        const hljs11 = core.default;
        hljs11.registerLanguage("cedar", cedar.hljsCedar);
        blocks.forEach(function (block) {
            hljs11.highlightElement(block);
        });
    });
})();
