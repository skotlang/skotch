; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [9 x i8] c"Saturday\00", align 1
@.str.1 = private unnamed_addr constant [8 x i8] c"weekend\00", align 1
@.str.2 = private unnamed_addr constant [7 x i8] c"Sunday\00", align 1
@.str.3 = private unnamed_addr constant [8 x i8] c"weekday\00", align 1
@.str.4 = private unnamed_addr constant [7 x i8] c"Monday\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)
declare i32 @strcmp(ptr, ptr)

define ptr @InputKt_dayType(ptr %arg0) {
entry:
  %merge_2 = alloca ptr
  br label %bb1
bb1:
  %t0 = call i32 @strcmp(ptr %arg0, ptr @.str.0)
  %t1 = icmp eq i32 %t0, 0
  br i1 %t1, label %bb2, label %bb3
bb2:
  store ptr @.str.1, ptr %merge_2
  br label %bb6
bb3:
  %t2 = call i32 @strcmp(ptr %arg0, ptr @.str.2)
  %t3 = icmp eq i32 %t2, 0
  br i1 %t3, label %bb4, label %bb5
bb4:
  store ptr @.str.1, ptr %merge_2
  br label %bb6
bb5:
  store ptr @.str.3, ptr %merge_2
  br label %bb6
bb6:
  %t4 = load ptr, ptr %merge_2
  ret ptr %t4
}

define i32 @main() {
entry:
  %t0 = call ptr @InputKt_dayType(ptr @.str.4)
  call i32 @puts(ptr %t0)
  %t2 = call ptr @InputKt_dayType(ptr @.str.0)
  call i32 @puts(ptr %t2)
  %t4 = call ptr @InputKt_dayType(ptr @.str.2)
  call i32 @puts(ptr %t4)
  ret i32 0
}

