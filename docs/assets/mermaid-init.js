// Draws ```mermaid code blocks as diagrams. The library is loaded only on
// pages that have one, from this site (no third-party requests).
(function () {
    'use strict';
    var blocks = document.querySelectorAll('pre > code.language-mermaid');
    if (blocks.length === 0) { return; }

    var root = (typeof path_to_root === 'string') ? path_to_root : '';
    var sources = [];
    blocks.forEach(function (code) {
        var holder = document.createElement('div');
        holder.className = 'mermaid';
        holder.setAttribute('role', 'img');
        sources.push({ holder: holder, text: code.textContent });
        code.parentNode.replaceWith(holder);
    });

    function isDark() {
        var c = document.documentElement.classList;
        return c.contains('navy') || c.contains('coal') || c.contains('ayu');
    }

    var run = 0;
    function draw() {
        run += 1;
        window.mermaid.initialize({
            startOnLoad: false,
            securityLevel: 'strict',
            theme: isDark() ? 'dark' : 'neutral',
            fontFamily: 'inherit'
        });
        sources.forEach(function (s, i) {
            window.mermaid.render('rs-mermaid-' + run + '-' + i, s.text).then(function (out) {
                s.holder.innerHTML = out.svg;
            }).catch(function () {
                s.holder.textContent = s.text;
                s.holder.classList.add('mermaid-failed');
            });
        });
    }

    var script = document.createElement('script');
    script.src = root + 'assets/mermaid.min.js';
    script.onload = function () {
        draw();
        // Redraw when the reader switches between light and dark themes.
        new MutationObserver(draw).observe(document.documentElement, {
            attributes: true, attributeFilter: ['class']
        });
    };
    script.onerror = function () {
        sources.forEach(function (s) {
            var pre = document.createElement('pre');
            pre.textContent = s.text;
            s.holder.replaceWith(pre);
        });
    };
    document.head.appendChild(script);
})();
