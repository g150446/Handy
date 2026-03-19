import React from "react";

export interface ProgressData {
  id: string;
  percentage: number;
  speed?: number;
  label?: string;
  isIndeterminate?: boolean;
  downloaded?: number;
  total?: number;
}

interface ProgressBarProps {
  progress: ProgressData[];
  className?: string;
  size?: "small" | "medium" | "large";
  showSpeed?: boolean;
  showLabel?: boolean;
  showDetails?: boolean; // Show detailed progress (downloaded/total)
}

const ProgressBar: React.FC<ProgressBarProps> = ({
  progress,
  className = "",
  size = "medium",
  showSpeed = false,
  showLabel = false,
  showDetails = false,
}) => {
  const sizeClasses = {
    small: "w-16 h-1",
    medium: "w-20 h-1.5",
    large: "w-24 h-2",
  };

  const progressClasses = sizeClasses[size];

  const formatBytes = (bytes: number): string => {
    if (bytes === 0) return "0 B";
    const k = 1024;
    const sizes = ["B", "KB", "MB", "GB"];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
  };

  if (progress.length === 0) {
    return null;
  }

  if (progress.length === 1) {
    // Single progress bar
    const item = progress[0];
    const percentage = Math.max(0, Math.min(100, item.percentage));

    return (
      <div className={`flex items-center gap-3 ${className}`}>
        {item.isIndeterminate ? (
          <div
            className={`${progressClasses} rounded-full bg-mid-gray/20 overflow-hidden`}
          >
            <div className="h-full w-1/3 rounded-full bg-logo-primary animate-indeterminate" />
          </div>
        ) : (
          <progress
            value={percentage}
            max={100}
            className={`${progressClasses} [&::-webkit-progress-bar]:rounded-full [&::-webkit-progress-bar]:bg-mid-gray/20 [&::-webkit-progress-value]:rounded-full [&::-webkit-progress-value]:bg-logo-primary`}
          />
        )}
        {(showSpeed || showLabel || showDetails) && (
          <div className="text-xs text-text/60 tabular-nums min-w-fit flex flex-col">
            <div className="flex items-center gap-2">
              {showLabel && item.label && (
                <span className="font-medium">{item.label}</span>
              )}
              {!item.isIndeterminate && (
                <span className="font-medium">{percentage.toFixed(1)}%</span>
              )}
            </div>
            <div className="flex items-center gap-2 text-[10px]">
              {showDetails && item.downloaded !== undefined && item.total !== undefined && (
                <span>{formatBytes(item.downloaded)} / {formatBytes(item.total)}</span>
              )}
              {showSpeed && item.speed !== undefined && item.speed > 0 && (
                <span>{item.speed.toFixed(1)} MB/s</span>
              )}
              {item.isIndeterminate && (
                <span>Starting...</span>
              )}
            </div>
          </div>
        )}
      </div>
    );
  }

  // Multiple progress bars
  return (
    <div className={`flex items-center gap-2 ${className}`}>
      <div className="flex gap-1">
        {progress.map((item) => {
          const percentage = Math.max(0, Math.min(100, item.percentage));
          return item.isIndeterminate ? (
            <div
              key={item.id}
              title={item.label || "Downloading..."}
              className="w-3 h-1.5 rounded-full bg-mid-gray/20 overflow-hidden"
            >
              <div className="h-full w-1/2 rounded-full bg-logo-primary animate-indeterminate" />
            </div>
          ) : (
            <progress
              key={item.id}
              value={percentage}
              max={100}
              title={item.label || `${percentage}%`}
              className="w-3 h-1.5 [&::-webkit-progress-bar]:rounded-full [&::-webkit-progress-bar]:bg-mid-gray/20 [&::-webkit-progress-value]:rounded-full [&::-webkit-progress-value]:bg-logo-primary"
            />
          );
        })}
      </div>
      <div className="text-xs text-text/60 min-w-fit">
        {progress.length} downloading...
      </div>
    </div>
  );
};

export default ProgressBar;
