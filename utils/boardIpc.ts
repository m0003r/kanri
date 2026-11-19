// SPDX-License-Identifier: GPL-3.0-or-later

import { invoke, isTauri } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { platform } from "@tauri-apps/plugin-os";
import { z } from "zod";
import type { useBoardsStore } from "@/stores/boards";
import { kanriCardSchema } from "@/types/json-schemas";
import type { Card } from "@/types/kanban-types";

const id = z.string().min(1).max(512);
const cardFields = kanriCardSchema.omit({ id: true }).partial().extend({
  name: z.string().trim().min(1).optional(),
  dueDate: z.string().refine(value => Number.isFinite(Date.parse(value))).nullish(),
  isDueDateCompleted: z.boolean().optional(),
  isDueDateCounterRelative: z.boolean().optional(),
}).strict();
const requestSchema = z.discriminatedUnion("op", [
  z.object({ op: z.literal("board.list") }).strict(),
  z.object({ op: z.literal("board.get"), boardId: id }).strict(),
  z.object({
    op: z.literal("card.upsert"),
    boardId: id,
    cardId: id,
    columnId: id.optional(),
    position: z.number().int().nonnegative().optional(),
    fields: cardFields.optional(),
  }).strict(),
]);

type BoardsStore = ReturnType<typeof useBoardsStore>;
type Reply = { ok: true; result: unknown } | { ok: false; code: string; error: string };

function failure(code: string, error: string): Reply {
  return { ok: false, code, error };
}

export async function applyBoardCommand(store: BoardsStore, input: unknown): Promise<Reply> {
  const parsed = requestSchema.safeParse(input);
  if (!parsed.success) {
    return failure("invalid_request", parsed.error.issues.map(issue =>
      [issue.path.join("."), issue.message].filter(Boolean).join(" "),
    ).join("; "));
  }
  const command = parsed.data;
  if (command.op === "board.list") {
    const boards = store.boards.map(({ id, title, columns }) => ({
      id, title, columns: columns.map(({ id, title }) => ({ id, title })),
    }));
    return { ok: true, result: boards };
  }

  const board = store.boardById(command.boardId);
  if (!board) return failure("board_not_found", "No board with that ID");
  if (command.op === "board.get") return { ok: true, result: board };

  const { cardId, columnId, position, fields } = command;
  const matches = board.columns.flatMap(column =>
    column.cards.filter(card => card.id === cardId).map(card => ({ column, card })),
  );
  if (matches.length > 1) return failure("ambiguous_card", "The board contains duplicate card IDs");
  const existing = matches[0];
  const destination = columnId ? board.columns.find(column => column.id === columnId) : existing?.column;
  if (!destination) return failure("column_not_found", "Specify the ID of a column on that board");

  let card: Card;
  if (existing) {
    card = existing.card;
    Object.assign(card, fields);
    if (existing.column !== destination || position !== undefined) {
      existing.column.cards.splice(existing.column.cards.indexOf(card), 1);
      destination.cards.splice(position ?? destination.cards.length, 0, card);
    }
  } else {
    if (!fields?.name) return failure("missing_name", "A new card needs fields.name");
    card = { ...fields, id: cardId, name: fields.name };
    destination.cards.splice(position ?? destination.cards.length, 0, card);
  }
  board.lastEdited = new Date();
  try {
    await store.save();
  } catch {
    return failure("save_failed", "The board changed in memory, but Kanri could not save it");
  }
  return { ok: true, result: { id: cardId, boardId: board.id, columnId: destination.id } };
}

export async function startBoardIpc(store: BoardsStore) {
  if (!isTauri() || !["macos", "linux"].includes(platform())) return;
  const stop = await getCurrentWebviewWindow().listen<{ id: number; command: unknown }>(
    "board-ipc-request",
    async ({ payload: { id, command } }) => {
      let reply: Reply;
      try {
        reply = await applyBoardCommand(store, command);
      } catch {
        reply = failure("internal_error", "Kanri could not apply the request");
      }
      try {
        await invoke("finish_board_ipc", { id, reply });
      } catch (error) {
        console.error("Kanri IPC could not send its reply:", error);
      }
    },
  );
  try {
    await invoke("start_board_ipc");
  } catch (error) {
    stop();
    throw error;
  }
}
