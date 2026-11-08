import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

afterEach(() => cleanup());

// jsdom has no layout; components scroll rows and console lines into view.
Element.prototype.scrollIntoView = function () {};
