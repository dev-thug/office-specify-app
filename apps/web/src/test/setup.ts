import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(async () => {
  cleanup();
  // Radix FocusScope dispatches its unmount autofocus CustomEvent in a
  // zero-delay timer. Drain that timer while the owning jsdom window is still
  // active so another test file cannot replace the global event constructor.
  await new Promise<void>((resolve) => window.setTimeout(resolve, 0));
});
