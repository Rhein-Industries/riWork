import { expect, test } from "nsh/test";
import { SETTINGS_SECTIONS, modelIdError, sectionColumns, settingsLayout, themeColumns } from "./settings_model";

test("P6a settings width classes preserve the nine-section keyboard order", () => {
  for (const width of [0, 650, 700, 1199, 1200, 1700]) expect(sectionColumns(width).flat()).toEqual(SETTINGS_SECTIONS.map((section) => section.number));
  expect(settingsLayout(699).kind).toBe("narrow"); expect(settingsLayout(700).maxWidth).toBe(820); expect(settingsLayout(1200).maxWidth).toBe(1320);
  expect(sectionColumns(1200)).toEqual([["01", "02", "03", "04"], ["05", "06", "07", "08", "09"]]);
  expect(themeColumns(650, 1)).toBe(1); expect(themeColumns(820, 1)).toBe(3); expect(themeColumns(1200, 1)).toBe(2);
});
test("P6a custom orchestrator model validation never replaces an unreadable choice without a valid edit", () => {
  expect(modelIdError(" ")).toBe("Type a model ID first.");
  expect(modelIdError("a".repeat(100))).toBe(null);
  expect(modelIdError("a".repeat(101))).toBe("A model ID has at most 100 characters and no control characters.");
  expect(modelIdError("m\x85odel")).toBe("A model ID has at most 100 characters and no control characters.");
  expect(modelIdError(" custom-model ")).toBe(null);
});
