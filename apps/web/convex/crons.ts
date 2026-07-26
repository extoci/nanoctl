import { cronJobs } from "convex/server";
import { internal } from "./_generated/api";

const crons = cronJobs();

crons.interval("purge expired ephemeral data", { minutes: 15 }, internal.maintenance.purgeExpired);

export default crons;
