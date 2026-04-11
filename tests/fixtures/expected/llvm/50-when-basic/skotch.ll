; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [4 x i8] c"one\00", align 1
@.str.1 = private unnamed_addr constant [4 x i8] c"two\00", align 1
@.str.2 = private unnamed_addr constant [6 x i8] c"three\00", align 1
@.str.3 = private unnamed_addr constant [6 x i8] c"other\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_describe(i32 %arg0) {
entry:
  %merge_2 = alloca ptr
  br label %bb1
bb1:
  %t0 = add i32 0, 1
  %t1 = icmp eq i32 %arg0, %t0
  br i1 %t1, label %bb2, label %bb3
bb2:
  store ptr @.str.0, ptr %merge_2
  br label %bb8
bb3:
  %t2 = add i32 0, 2
  %t3 = icmp eq i32 %arg0, %t2
  br i1 %t3, label %bb4, label %bb5
bb4:
  store ptr @.str.1, ptr %merge_2
  br label %bb8
bb5:
  %t4 = add i32 0, 3
  %t5 = icmp eq i32 %arg0, %t4
  br i1 %t5, label %bb6, label %bb7
bb6:
  store ptr @.str.2, ptr %merge_2
  br label %bb8
bb7:
  store ptr @.str.3, ptr %merge_2
  br label %bb8
bb8:
  %t6 = load ptr, ptr %merge_2
  ret ptr %t6
}

define i32 @main() {
entry:
  %t0 = add i32 0, 1
  %t1 = call ptr @InputKt_describe(i32 %t0)
  call i32 @puts(ptr %t1)
  %t3 = add i32 0, 2
  %t4 = call ptr @InputKt_describe(i32 %t3)
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, 99
  %t7 = call ptr @InputKt_describe(i32 %t6)
  call i32 @puts(ptr %t7)
  ret i32 0
}

