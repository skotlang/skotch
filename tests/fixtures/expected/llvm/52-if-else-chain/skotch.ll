; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [9 x i8] c"negative\00", align 1
@.str.1 = private unnamed_addr constant [5 x i8] c"zero\00", align 1
@.str.2 = private unnamed_addr constant [9 x i8] c"positive\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_classify(i32 %arg0) {
entry:
  %merge_4 = alloca ptr
  %merge_9 = alloca ptr
  %t0 = add i32 0, 0
  %t1 = icmp slt i32 %arg0, %t0
  br i1 %t1, label %bb1, label %bb2
bb1:
  store ptr @.str.0, ptr %merge_4
  br label %bb3
bb2:
  %t2 = add i32 0, 0
  %t3 = icmp eq i32 %arg0, %t2
  br i1 %t3, label %bb4, label %bb5
bb3:
  %t4 = load ptr, ptr %merge_4
  ret ptr %t4
bb4:
  store ptr @.str.1, ptr %merge_9
  br label %bb6
bb5:
  store ptr @.str.2, ptr %merge_9
  br label %bb6
bb6:
  %t5 = load ptr, ptr %merge_9
  store ptr %t5, ptr %merge_4
  br label %bb3
}

define i32 @main() {
entry:
  %t0 = add i32 0, -5
  %t1 = call ptr @InputKt_classify(i32 %t0)
  call i32 @puts(ptr %t1)
  %t3 = add i32 0, 0
  %t4 = call ptr @InputKt_classify(i32 %t3)
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, 42
  %t7 = call ptr @InputKt_classify(i32 %t6)
  call i32 @puts(ptr %t7)
  ret i32 0
}

