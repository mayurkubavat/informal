import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup, configure } from "@testing-library/react";

afterEach(() => cleanup());

// findBy* and waitFor give up after 1 s by default: too short on a busy machine.
configure({ asyncUtilTimeout: 10_000 });

// jsdom has no layout; components scroll rows and console lines into view.
Element.prototype.scrollIntoView = function () {};
