// Marks the "Level:" line that opens each page so the stylesheet can box it.
(function () {
    'use strict';
    var first = document.querySelector('.content main h1 + p');
    if (first && /^Level:/.test(first.textContent.trim())) {
        first.classList.add('level-line');
    }
})();
